//! Admission between the current-v1 snapshot and canonical session store.
//!
//! The decision table is pure. Its observation seam performs bounded,
//! descriptor-pinned reads only: it never creates, repairs, syncs, locks,
//! migrates, selects, or removes either representation.

use std::ffi::OsStr;
use std::io::Read;

use kitsu_core::session::{
    LogicalHead, RevisionId, SessionCommitOutcome, SessionId, SessionLookupState, StoreId,
};

use super::canonical::{
    append_complete_operation, project_canonical_resume_v1, CanonicalOperationInputV1,
    CanonicalResumeProjectionErrorV1, CanonicalResumeProjectionV1, CanonicalSessionRevisionV1,
};
use super::physical::{PhysicalFileStoreV1, PhysicalReadErrorV1, PhysicalWriterFenceV1};
use super::transaction::{
    commit_canonical_mount_payload_under_fence_v1, lookup_canonical_mount_value_v1,
    CanonicalTransactionErrorV1,
};
use super::{
    PrivateDirectory, PrivateEntryKind, SavedSession, SessionLease, MAX_SESSION_BYTES,
    SESSION_VERSION,
};

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

/// A canonical resume value whose exact source remains selected while both the
/// legacy session lease and canonical writer fence are owned. The physical
/// store stays descriptor-pinned for the activation lifetime.
#[derive(Debug)]
pub(super) struct CanonicalMountActivationV1 {
    projection: CanonicalResumeProjectionV1,
    base: CanonicalSessionRevisionV1,
    _legacy_lease: SessionLease,
    canonical_fence: PhysicalWriterFenceV1,
    store: PhysicalFileStoreV1,
}

#[derive(Debug)]
pub(super) struct CurrentV1MountActivationV1 {
    value: CurrentV1MountValueV1,
    _legacy_lease: SessionLease,
    _canonical_fence: PhysicalWriterFenceV1,
    _store: PhysicalFileStoreV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SessionMountActivatedSourceV1 {
    CurrentV1,
    Canonical {
        revision: RevisionId,
        head: LogicalHead,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SessionMountActivationReceiptV1 {
    store: StoreId,
    session: SessionId,
    source: SessionMountActivatedSourceV1,
}

impl SessionMountActivationReceiptV1 {
    pub(super) fn store(&self) -> &StoreId {
        &self.store
    }

    pub(super) fn session(&self) -> &SessionId {
        &self.session
    }

    pub(super) fn source(&self) -> &SessionMountActivatedSourceV1 {
        &self.source
    }
}

#[derive(Debug)]
enum SessionMountActivationInnerV1 {
    CurrentV1(Box<CurrentV1MountActivationV1>),
    Canonical(Box<CanonicalMountActivationV1>),
}

/// Total owner of one exact, dual-fenced resume value. It deliberately has no
/// consuming snapshot extractor: production must retain this owner while using
/// the borrowed resume value. An explicit snapshot clone remains data only and
/// is never activation or HEAD evidence.
#[derive(Debug)]
pub(super) struct SessionMountActivationV1 {
    receipt: SessionMountActivationReceiptV1,
    inner: SessionMountActivationInnerV1,
}

impl SessionMountActivationV1 {
    pub(super) fn saved(&self) -> &SavedSession {
        match &self.inner {
            SessionMountActivationInnerV1::CurrentV1(activation) => activation.value().saved(),
            SessionMountActivationInnerV1::Canonical(activation) => activation.projection().saved(),
        }
    }

    pub(super) fn receipt(&self) -> &SessionMountActivationReceiptV1 {
        &self.receipt
    }

    pub(super) fn save_canonical_operation(
        &mut self,
        revision_id: RevisionId,
        committed_at: String,
        input: CanonicalOperationInputV1,
    ) -> Result<CanonicalMountSaveReceiptV1, SessionMountSaveErrorV1> {
        match &mut self.inner {
            SessionMountActivationInnerV1::CurrentV1(_) => {
                Err(SessionMountSaveErrorV1::CurrentV1SnapshotRequired)
            }
            SessionMountActivationInnerV1::Canonical(activation) => activation
                .save_operation(revision_id, committed_at, input)
                .map_err(SessionMountSaveErrorV1::Canonical),
        }
    }
}

impl CurrentV1MountActivationV1 {
    pub(super) fn value(&self) -> &CurrentV1MountValueV1 {
        &self.value
    }
}

impl CanonicalMountActivationV1 {
    pub(super) fn projection(&self) -> &CanonicalResumeProjectionV1 {
        &self.projection
    }

    fn save_operation(
        &mut self,
        revision_id: RevisionId,
        committed_at: String,
        input: CanonicalOperationInputV1,
    ) -> Result<CanonicalMountSaveReceiptV1, CanonicalMountSaveErrorV1> {
        // Assembly and projection are pure. In particular, an operation that
        // introduces artifacts is refused before a candidate can be installed.
        let next = append_complete_operation(&self.base, revision_id, committed_at, input)
            .map_err(CanonicalMountSaveErrorV1::Assembly)?;
        let next_projection =
            project_canonical_resume_v1(&next).map_err(CanonicalMountSaveErrorV1::Projection)?;
        let commit = commit_canonical_mount_payload_under_fence_v1(
            &self.store,
            &self.canonical_fence,
            &self.base,
            &next,
        )
        .map_err(CanonicalMountSaveErrorV1::Transaction)?;
        let receipt = CanonicalMountSaveReceiptV1 {
            outcome: commit.outcome().clone(),
            durability_warning: commit.durability_warning().map(str::to_string),
        };
        self.base = next;
        self.projection = next_projection;
        Ok(receipt)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CanonicalMountSaveReceiptV1 {
    outcome: SessionCommitOutcome,
    durability_warning: Option<String>,
}

impl CanonicalMountSaveReceiptV1 {
    pub(super) fn outcome(&self) -> &SessionCommitOutcome {
        &self.outcome
    }

    pub(super) fn durability_warning(&self) -> Option<&str> {
        self.durability_warning.as_deref()
    }
}

#[derive(Debug)]
pub(super) enum CanonicalMountSaveErrorV1 {
    Assembly(String),
    Projection(CanonicalResumeProjectionErrorV1),
    Transaction(CanonicalTransactionErrorV1),
}

#[derive(Debug)]
pub(super) enum SessionMountSaveErrorV1 {
    CurrentV1SnapshotRequired,
    Canonical(CanonicalMountSaveErrorV1),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CanonicalMountActivationErrorV1 {
    WrongSelection,
    SelectionIdentityMismatch,
    Projection(CanonicalResumeProjectionErrorV1),
    LegacyFence(String),
    StoreUnavailable(SessionLookupState),
    CanonicalFence(String),
    CurrentV1Changed(CurrentV1MountObservationV1),
    CanonicalChanged(SessionLookupState),
    CanonicalValueChanged {
        revision: RevisionId,
        head: LogicalHead,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CurrentV1MountActivationErrorV1 {
    WrongSelection,
    SelectionIdentityMismatch,
    LegacyFence(String),
    StoreInitialization(String),
    CanonicalFence(String),
    CurrentV1Changed(CurrentV1MountObservationV1),
    CurrentV1ValueChanged,
    CanonicalChanged(SessionLookupState),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SessionMountActivationErrorV1 {
    Empty,
    Rejected(SessionMountAdmissionV1),
    CurrentV1(CurrentV1MountActivationErrorV1),
    Canonical(CanonicalMountActivationErrorV1),
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

pub(super) fn activate_canonical_mount_v1(
    session_root: &PrivateDirectory,
    expected_store: StoreId,
    session_id: &SessionId,
    selection: SessionMountSelectionV1,
) -> Result<CanonicalMountActivationV1, CanonicalMountActivationErrorV1> {
    let SessionMountSelectionV1::Canonical(selected) = selection else {
        return Err(CanonicalMountActivationErrorV1::WrongSelection);
    };
    if selected.store_id() != &expected_store || selected.session_id() != session_id {
        return Err(CanonicalMountActivationErrorV1::SelectionIdentityMismatch);
    }
    // Projection is pure and runs before either lock file can be created. In
    // particular, unsupported artifact-bearing history fails with zero effects.
    let projection = project_canonical_resume_v1(&selected)
        .map_err(CanonicalMountActivationErrorV1::Projection)?;

    // Every dual-format activation acquires these in one stable order. The
    // legacy lease excludes current-v1 writers; the canonical fence excludes
    // HEAD/revision writers. Both remain owned by the returned activation.
    let legacy_lease = SessionLease::acquire(session_root, session_id.as_str())
        .map_err(CanonicalMountActivationErrorV1::LegacyFence)?;
    let store = match PhysicalFileStoreV1::open_existing(session_root, expected_store) {
        Ok(Some(store)) => store,
        Ok(None) => {
            return Err(CanonicalMountActivationErrorV1::StoreUnavailable(
                SessionLookupState::Missing {
                    tombstone_revision: None,
                },
            ));
        }
        Err(error) => {
            return Err(CanonicalMountActivationErrorV1::StoreUnavailable(
                physical_store_error_state(session_id, error),
            ));
        }
    };
    let canonical_fence = store
        .acquire_writer_fence(session_id)
        .map_err(CanonicalMountActivationErrorV1::CanonicalFence)?;

    let current_v1 = observe_current_v1(session_root, session_id).observation;
    if current_v1 != CurrentV1MountObservationV1::Missing {
        return Err(CanonicalMountActivationErrorV1::CurrentV1Changed(
            current_v1,
        ));
    }

    let (state, observed) = lookup_canonical_mount_value_v1(&store, session_id);
    let expected_state = SessionLookupState::Present {
        revision: selected.revision_id().clone(),
        head: selected.selected_head().clone(),
    };
    if state != expected_state {
        return Err(CanonicalMountActivationErrorV1::CanonicalChanged(state));
    }
    let Some(observed) = observed else {
        return Err(CanonicalMountActivationErrorV1::CanonicalChanged(state));
    };
    if observed != *selected {
        return Err(CanonicalMountActivationErrorV1::CanonicalValueChanged {
            revision: observed.revision_id().clone(),
            head: observed.selected_head().clone(),
        });
    }

    Ok(CanonicalMountActivationV1 {
        projection,
        base: *selected,
        _legacy_lease: legacy_lease,
        canonical_fence,
        store,
    })
}

pub(super) fn activate_current_v1_mount_v1(
    session_root: &PrivateDirectory,
    expected_store: StoreId,
    session_id: &SessionId,
    selection: SessionMountSelectionV1,
) -> Result<CurrentV1MountActivationV1, CurrentV1MountActivationErrorV1> {
    let SessionMountSelectionV1::CurrentV1(selected) = selection else {
        return Err(CurrentV1MountActivationErrorV1::WrongSelection);
    };
    if selected.saved().id != session_id.as_str() {
        return Err(CurrentV1MountActivationErrorV1::SelectionIdentityMismatch);
    }

    let legacy_lease = SessionLease::acquire(session_root, session_id.as_str())
        .map_err(CurrentV1MountActivationErrorV1::LegacyFence)?;
    // Creating the disjoint store while the legacy lease is held gives a
    // current-only session a canonical lock domain. A concurrent canonical
    // publisher either wins before the fence and is observed below, holds the
    // fence and blocks activation, or loses after this activation owns it.
    let store = PhysicalFileStoreV1::create_or_open_pinned(session_root, expected_store)
        .map_err(CurrentV1MountActivationErrorV1::StoreInitialization)?;
    let canonical_fence = store
        .acquire_writer_fence(session_id)
        .map_err(CurrentV1MountActivationErrorV1::CanonicalFence)?;

    let current = observe_current_v1(session_root, session_id);
    if current.observation != CurrentV1MountObservationV1::Present {
        return Err(CurrentV1MountActivationErrorV1::CurrentV1Changed(
            current.observation,
        ));
    }
    let Some(observed) = current.value else {
        return Err(CurrentV1MountActivationErrorV1::CurrentV1Changed(
            CurrentV1MountObservationV1::Present,
        ));
    };
    if observed.source_bytes() != selected.source_bytes() {
        return Err(CurrentV1MountActivationErrorV1::CurrentV1ValueChanged);
    }

    let (canonical, value) = lookup_canonical_mount_value_v1(&store, session_id);
    let expected = SessionLookupState::Missing {
        tombstone_revision: None,
    };
    if canonical != expected || value.is_some() {
        return Err(CurrentV1MountActivationErrorV1::CanonicalChanged(canonical));
    }

    Ok(CurrentV1MountActivationV1 {
        value: selected,
        _legacy_lease: legacy_lease,
        _canonical_fence: canonical_fence,
        _store: store,
    })
}

pub(super) fn activate_session_mount_v1(
    session_root: &PrivateDirectory,
    expected_store: StoreId,
    session_id: &SessionId,
    selection: SessionMountSelectionV1,
) -> Result<SessionMountActivationV1, SessionMountActivationErrorV1> {
    let (source, inner) = match selection {
        SessionMountSelectionV1::Empty => {
            return Err(SessionMountActivationErrorV1::Empty);
        }
        SessionMountSelectionV1::Rejected(admission) => {
            return Err(SessionMountActivationErrorV1::Rejected(admission));
        }
        SessionMountSelectionV1::CurrentV1(value) => {
            let activation = activate_current_v1_mount_v1(
                session_root,
                expected_store.clone(),
                session_id,
                SessionMountSelectionV1::CurrentV1(value),
            )
            .map_err(SessionMountActivationErrorV1::CurrentV1)?;
            (
                SessionMountActivatedSourceV1::CurrentV1,
                SessionMountActivationInnerV1::CurrentV1(Box::new(activation)),
            )
        }
        SessionMountSelectionV1::Canonical(value) => {
            let activation = activate_canonical_mount_v1(
                session_root,
                expected_store.clone(),
                session_id,
                SessionMountSelectionV1::Canonical(value),
            )
            .map_err(SessionMountActivationErrorV1::Canonical)?;
            let source = SessionMountActivatedSourceV1::Canonical {
                revision: activation.projection().source_revision().clone(),
                head: activation.projection().source_head().clone(),
            };
            (
                source,
                SessionMountActivationInnerV1::Canonical(Box::new(activation)),
            )
        }
    };

    Ok(SessionMountActivationV1 {
        receipt: SessionMountActivationReceiptV1 {
            store: expected_store,
            session: session_id.clone(),
            source,
        },
        inner,
    })
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
        activate_canonical_mount_v1, activate_current_v1_mount_v1, activate_session_mount_v1,
        admit_session_mount_v1, observe_session_mount_v1, select_session_mount_v1,
        CanonicalMountActivationErrorV1, CanonicalMountSaveErrorV1,
        CurrentV1MountActivationErrorV1, CurrentV1MountFaultV1, CurrentV1MountObservationV1,
        SessionMountActivatedSourceV1, SessionMountActivationErrorV1, SessionMountAdmissionV1,
        SessionMountRejectionV1, SessionMountSaveErrorV1, SessionMountSelectionV1,
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

    #[test]
    fn canonical_activation_revalidates_selection_under_both_session_fences() {
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
        let selection = select_session_mount_v1(&directory, store_id.clone(), &session);

        let activation =
            activate_canonical_mount_v1(&directory, store_id.clone(), &session, selection).unwrap();
        assert_eq!(activation.projection().source_revision().as_str(), "rev-a");
        assert_eq!(activation.projection().source_head(), &LogicalHead::Genesis);
        assert_eq!(activation.projection().saved().id, "session-a");
        assert!(
            super::super::SessionLease::acquire(&directory, session.as_str())
                .unwrap_err()
                .contains("already active")
        );
        let competing_store = super::super::physical::PhysicalFileStoreV1::open_existing(
            &directory,
            store_id.clone(),
        )
        .unwrap()
        .unwrap();
        assert!(competing_store
            .acquire_writer_fence(&session)
            .unwrap_err()
            .contains("already active"));

        drop(activation);
        let legacy = super::super::SessionLease::acquire(&directory, session.as_str()).unwrap();
        drop(legacy);
        let canonical = competing_store.acquire_writer_fence(&session).unwrap();
        drop(canonical);
    }

    #[test]
    fn canonical_activation_refuses_current_or_canonical_change_after_selection() {
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

        let current_race = select_session_mount_v1(&directory, store_id.clone(), &session);
        std::fs::write(session_root.join("session-a.json"), &source).unwrap();
        assert_eq!(
            activate_canonical_mount_v1(&directory, store_id.clone(), &session, current_race)
                .unwrap_err(),
            CanonicalMountActivationErrorV1::CurrentV1Changed(CurrentV1MountObservationV1::Present)
        );
        assert_eq!(
            std::fs::read(session_root.join("session-a.json")).unwrap(),
            source
        );
        std::fs::remove_file(session_root.join("session-a.json")).unwrap();

        let canonical_race = select_session_mount_v1(&directory, store_id.clone(), &session);
        let payload_path = super::super::physical::PhysicalLayoutV1::canonical_payload(
            &session_root,
            &session,
            &revision,
        );
        std::fs::write(&payload_path, b"{").unwrap();
        assert!(matches!(
            activate_canonical_mount_v1(&directory, store_id, &session, canonical_race),
            Err(CanonicalMountActivationErrorV1::CanonicalChanged(
                SessionLookupState::Corrupt {
                    observed_revision: Some(ref observed),
                    ..
                }
            )) if observed == &revision
        ));
        assert_eq!(std::fs::read(payload_path).unwrap(), b"{");
    }

    #[test]
    fn canonical_activation_refuses_a_new_head_published_after_selection() {
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
        let selection = select_session_mount_v1(&directory, store_id.clone(), &session);
        let SessionMountSelectionV1::Canonical(selected) = &selection else {
            panic!("expected selected canonical value")
        };

        let store = super::super::physical::PhysicalFileStoreV1::open_existing(
            &directory,
            store_id.clone(),
        )
        .unwrap()
        .unwrap();
        super::super::transaction::publish_mount_successor_fixture_v1(
            &store,
            selected,
            revision("rev-b"),
            "2026-08-20T10:03:00.000Z".to_string(),
        )
        .unwrap();
        drop(store);

        assert!(matches!(
            activate_canonical_mount_v1(&directory, store_id, &session, selection),
            Err(CanonicalMountActivationErrorV1::CanonicalChanged(
                SessionLookupState::Present { revision, head }
            )) if revision.as_str() == "rev-b"
                && head == LogicalHead::Turn(TurnId::new("turn-mount-activation").unwrap())
        ));
    }

    #[test]
    fn current_activation_initializes_pinned_store_and_revalidates_both_domains() {
        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        std::fs::create_dir(&session_root).unwrap();
        let store_id = StoreId::new("store-a").unwrap();
        let session = SessionId::new("session-a").unwrap();
        let source = current_v1_bytes(1, session.as_str());
        std::fs::write(session_root.join("session-a.json"), &source).unwrap();
        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();
        let selection = select_session_mount_v1(&directory, store_id.clone(), &session);
        assert!(!session_root.join(".kitsu-store-v1").exists());

        let activation =
            activate_current_v1_mount_v1(&directory, store_id.clone(), &session, selection)
                .unwrap();
        assert_eq!(activation.value().saved().id, "session-a");
        assert_eq!(activation.value().source_bytes(), source);
        assert!(session_root.join(".kitsu-store-v1").is_dir());
        assert!(
            super::super::SessionLease::acquire(&directory, session.as_str())
                .unwrap_err()
                .contains("already active")
        );
        let competing_store =
            super::super::physical::PhysicalFileStoreV1::open_existing(&directory, store_id)
                .unwrap()
                .unwrap();
        assert!(competing_store
            .acquire_writer_fence(&session)
            .unwrap_err()
            .contains("already active"));
        assert_eq!(
            std::fs::read(session_root.join("session-a.json")).unwrap(),
            source
        );

        drop(activation);
        let legacy = super::super::SessionLease::acquire(&directory, session.as_str()).unwrap();
        drop(legacy);
        drop(competing_store.acquire_writer_fence(&session).unwrap());
    }

    #[test]
    fn current_activation_refuses_replaced_bytes_or_new_canonical_head() {
        let root = tempfile::tempdir().unwrap();
        let replaced_root = root.path().join("replaced");
        std::fs::create_dir(&replaced_root).unwrap();
        let store_id = StoreId::new("store-a").unwrap();
        let session = SessionId::new("session-a").unwrap();
        let source = current_v1_bytes(1, session.as_str());
        let current_path = replaced_root.join("session-a.json");
        std::fs::write(&current_path, &source).unwrap();
        let directory =
            super::super::PrivateDirectory::open(&replaced_root, "mount fixture").unwrap();
        let selection = select_session_mount_v1(&directory, store_id.clone(), &session);
        let mut replacement: serde_json::Value = serde_json::from_slice(&source).unwrap();
        replacement["model"] = serde_json::json!("model-replaced");
        let replacement = serde_json::to_vec(&replacement).unwrap();
        std::fs::write(&current_path, &replacement).unwrap();
        assert_eq!(
            activate_current_v1_mount_v1(&directory, store_id.clone(), &session, selection)
                .unwrap_err(),
            CurrentV1MountActivationErrorV1::CurrentV1ValueChanged
        );
        assert_eq!(std::fs::read(&current_path).unwrap(), replacement);

        let canonical_root = root.path().join("canonical-appeared");
        std::fs::create_dir(&canonical_root).unwrap();
        std::fs::write(canonical_root.join("session-a.json"), &source).unwrap();
        let canonical_directory =
            super::super::PrivateDirectory::open(&canonical_root, "mount fixture").unwrap();
        let selection = select_session_mount_v1(&canonical_directory, store_id.clone(), &session);
        let store = super::super::physical::PhysicalFileStoreV1::create_or_open_pinned(
            &canonical_directory,
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
        assert!(matches!(
            activate_current_v1_mount_v1(
                &canonical_directory,
                store_id,
                &session,
                selection
            ),
            Err(CurrentV1MountActivationErrorV1::CanonicalChanged(
                SessionLookupState::Present { revision, head }
            )) if revision.as_str() == "rev-a" && head == LogicalHead::Genesis
        ));
        assert_eq!(
            std::fs::read(canonical_root.join("session-a.json")).unwrap(),
            source
        );

        let partial_root = root.path().join("partial-appeared");
        std::fs::create_dir(&partial_root).unwrap();
        std::fs::write(
            partial_root.join("session-a.json"),
            current_v1_bytes(1, "session-a"),
        )
        .unwrap();
        let partial_directory =
            super::super::PrivateDirectory::open(&partial_root, "mount fixture").unwrap();
        let selection = select_session_mount_v1(
            &partial_directory,
            StoreId::new("store-a").unwrap(),
            &session,
        );
        let store_root = partial_root.join(".kitsu-store-v1");
        std::fs::create_dir(&store_root).unwrap();
        assert!(matches!(
            activate_current_v1_mount_v1(
                &partial_directory,
                StoreId::new("store-a").unwrap(),
                &session,
                selection
            ),
            Err(CurrentV1MountActivationErrorV1::StoreInitialization(_))
        ));
        assert_eq!(
            std::fs::read_dir(&store_root).unwrap().count(),
            0,
            "activation must not repair a partial concurrent canonical root"
        );
    }

    #[test]
    fn total_activation_owner_borrows_resume_value_and_preserves_source_receipt() {
        let root = tempfile::tempdir().unwrap();
        let store_id = StoreId::new("store-a").unwrap();
        let session = SessionId::new("session-a").unwrap();
        let source = current_v1_bytes(1, session.as_str());

        let current_root = root.path().join("current");
        std::fs::create_dir(&current_root).unwrap();
        std::fs::write(current_root.join("session-a.json"), &source).unwrap();
        let current_directory =
            super::super::PrivateDirectory::open(&current_root, "mount fixture").unwrap();
        let current_selection =
            select_session_mount_v1(&current_directory, store_id.clone(), &session);
        let current = activate_session_mount_v1(
            &current_directory,
            store_id.clone(),
            &session,
            current_selection,
        )
        .unwrap();
        assert_eq!(current.saved().id, "session-a");
        assert_eq!(current.receipt().store(), &store_id);
        assert_eq!(current.receipt().session(), &session);
        assert_eq!(
            current.receipt().source(),
            &SessionMountActivatedSourceV1::CurrentV1
        );
        assert!(super::super::SessionLease::acquire(&current_directory, session.as_str()).is_err());
        drop(current);

        let canonical_root = root.path().join("canonical");
        let store = super::super::physical::PhysicalFileStoreV1::create_or_open(
            &canonical_root,
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
        let canonical_directory =
            super::super::PrivateDirectory::open(&canonical_root, "mount fixture").unwrap();
        let canonical_selection =
            select_session_mount_v1(&canonical_directory, store_id.clone(), &session);
        let canonical = activate_session_mount_v1(
            &canonical_directory,
            store_id.clone(),
            &session,
            canonical_selection,
        )
        .unwrap();
        assert_eq!(canonical.saved().id, "session-a");
        assert_eq!(canonical.receipt().store(), &store_id);
        assert_eq!(canonical.receipt().session(), &session);
        assert_eq!(
            canonical.receipt().source(),
            &SessionMountActivatedSourceV1::Canonical {
                revision: revision("rev-a"),
                head: LogicalHead::Genesis,
            }
        );
        let competing_store = super::super::physical::PhysicalFileStoreV1::open_existing(
            &canonical_directory,
            store_id,
        )
        .unwrap()
        .unwrap();
        assert!(competing_store.acquire_writer_fence(&session).is_err());
        drop(canonical);
        drop(competing_store.acquire_writer_fence(&session).unwrap());
    }

    #[test]
    fn total_activation_preserves_empty_and_rejected_selection_evidence() {
        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        std::fs::create_dir(&session_root).unwrap();
        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();
        let store_id = StoreId::new("store-a").unwrap();
        let session = SessionId::new("session-a").unwrap();
        let before = tree_snapshot(&session_root);
        assert_eq!(
            activate_session_mount_v1(
                &directory,
                store_id.clone(),
                &session,
                SessionMountSelectionV1::Empty
            )
            .unwrap_err(),
            SessionMountActivationErrorV1::Empty
        );
        assert_eq!(tree_snapshot(&session_root), before);

        std::fs::write(session_root.join("session-a.json"), b"{").unwrap();
        let rejected = select_session_mount_v1(&directory, store_id.clone(), &session);
        let before_rejected = tree_snapshot(&session_root);
        assert!(matches!(
            activate_session_mount_v1(&directory, store_id, &session, rejected),
            Err(SessionMountActivationErrorV1::Rejected(
                SessionMountAdmissionV1::Rejected {
                    reason: SessionMountRejectionV1::UnavailableRepresentation,
                    current_v1: CurrentV1MountObservationV1::Unavailable(
                        CurrentV1MountFaultV1::Malformed
                    ),
                    canonical: SessionLookupState::Missing {
                        tombstone_revision: None
                    }
                }
            ))
        ));
        assert_eq!(tree_snapshot(&session_root), before_rejected);
    }

    #[test]
    fn canonical_total_activation_publishes_successor_under_its_owned_fence() {
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
        let selection = select_session_mount_v1(&directory, store_id.clone(), &session);
        let mut activation =
            activate_session_mount_v1(&directory, store_id.clone(), &session, selection).unwrap();

        let terminal =
            kitsu_core::RunSpec::new(kitsu_core::RunId::new("run-mounted-save-a").unwrap())
                .terminal(
                    kitsu_core::RunOutcomeClass::Succeeded,
                    kitsu_core::RunAssistantDisposition::Committed,
                );
        let input = super::super::canonical::CanonicalOperationInputV1::artifact_free_text(
            TurnId::new("turn-a").unwrap(),
            terminal,
            "first mounted input".to_string(),
            Some("first mounted answer".to_string()),
        );
        let saved = activation
            .save_canonical_operation(
                revision("rev-b"),
                "2026-08-20T10:03:00.000Z".to_string(),
                input,
            )
            .unwrap();
        assert_eq!(saved.durability_warning(), None);
        let kitsu_core::session::SessionCommitOutcome::Published { receipt } = saved.outcome()
        else {
            panic!("mounted canonical save must publish");
        };
        assert_eq!(receipt.store(), &store_id);
        assert_eq!(receipt.session(), &session);
        assert_eq!(
            receipt.base_revision(),
            &ExpectedRevision::Exact(revision("rev-a"))
        );
        assert_eq!(receipt.new_revision(), &revision("rev-b"));
        assert_eq!(
            receipt.head(),
            &LogicalHead::Turn(TurnId::new("turn-a").unwrap())
        );
        assert_eq!(activation.saved().updated_at, "2026-08-20T10:03:00.000Z");

        // The activation advances its exact base after publication, so a
        // second save chains from rev-b while the same fences remain owned.
        let terminal =
            kitsu_core::RunSpec::new(kitsu_core::RunId::new("run-mounted-save-b").unwrap())
                .terminal(
                    kitsu_core::RunOutcomeClass::Succeeded,
                    kitsu_core::RunAssistantDisposition::Committed,
                );
        let input = super::super::canonical::CanonicalOperationInputV1::artifact_free_text(
            TurnId::new("turn-b").unwrap(),
            terminal,
            "second mounted input".to_string(),
            Some("second mounted answer".to_string()),
        );
        let saved = activation
            .save_canonical_operation(
                revision("rev-c"),
                "2026-08-20T10:04:00.000Z".to_string(),
                input,
            )
            .unwrap();
        let kitsu_core::session::SessionCommitOutcome::Published { receipt } = saved.outcome()
        else {
            panic!("second mounted canonical save must publish");
        };
        assert_eq!(
            receipt.base_revision(),
            &ExpectedRevision::Exact(revision("rev-b"))
        );
        assert_eq!(receipt.new_revision(), &revision("rev-c"));
        assert_eq!(activation.saved().updated_at, "2026-08-20T10:04:00.000Z");

        let competing_store =
            super::super::physical::PhysicalFileStoreV1::open_existing(&directory, store_id)
                .unwrap()
                .unwrap();
        assert!(competing_store.acquire_writer_fence(&session).is_err());
        let (state, value) =
            super::super::transaction::lookup_canonical_mount_value_v1(&competing_store, &session);
        assert_eq!(
            state,
            SessionLookupState::Present {
                revision: revision("rev-c"),
                head: LogicalHead::Turn(TurnId::new("turn-b").unwrap()),
            }
        );
        assert_eq!(value.unwrap().revision_id(), &revision("rev-c"));
    }

    #[test]
    fn total_activation_routes_current_v1_save_without_creating_canonical_state() {
        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        std::fs::create_dir(&session_root).unwrap();
        let store_id = StoreId::new("store-a").unwrap();
        let session = SessionId::new("session-a").unwrap();
        std::fs::write(
            session_root.join("session-a.json"),
            current_v1_bytes(1, session.as_str()),
        )
        .unwrap();
        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();
        let selection = select_session_mount_v1(&directory, store_id, &session);
        let mut activation = activate_session_mount_v1(
            &directory,
            StoreId::new("store-a").unwrap(),
            &session,
            selection,
        )
        .unwrap();
        let before = tree_snapshot(&session_root);
        let terminal =
            kitsu_core::RunSpec::new(kitsu_core::RunId::new("run-current-save").unwrap()).terminal(
                kitsu_core::RunOutcomeClass::Succeeded,
                kitsu_core::RunAssistantDisposition::Committed,
            );
        let input = super::super::canonical::CanonicalOperationInputV1::artifact_free_text(
            TurnId::new("turn-a").unwrap(),
            terminal,
            "must remain current-v1".to_string(),
            Some("not canonically published".to_string()),
        );
        assert!(matches!(
            activation.save_canonical_operation(
                revision("rev-a"),
                "2026-08-20T10:02:00.000Z".to_string(),
                input,
            ),
            Err(SessionMountSaveErrorV1::CurrentV1SnapshotRequired)
        ));
        assert_eq!(tree_snapshot(&session_root), before);
    }

    #[test]
    fn canonical_activation_refuses_artifact_projection_before_allocation() {
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
        let selection = select_session_mount_v1(&directory, store_id.clone(), &session);
        let mut activation =
            activate_session_mount_v1(&directory, store_id, &session, selection).unwrap();
        let artifact = super::super::canonical::ImmutableArtifactRefV1::new(
            1,
            "artifact-a".to_string(),
            "00".repeat(32),
            7,
            Some("text/plain".to_string()),
        )
        .unwrap();
        let round = super::super::canonical::CanonicalToolRoundV1::new(
            "fs_read_file".to_string(),
            "{\"path\":\"a.txt\"}".to_string(),
            "call-a".to_string(),
            "content".to_string(),
            "effect-a".to_string(),
            artifact,
        );
        let terminal =
            kitsu_core::RunSpec::new(kitsu_core::RunId::new("run-artifact-save").unwrap())
                .terminal(
                    kitsu_core::RunOutcomeClass::Succeeded,
                    kitsu_core::RunAssistantDisposition::Committed,
                );
        let input = super::super::canonical::CanonicalOperationInputV1::artifact_tool_round(
            TurnId::new("turn-a").unwrap(),
            terminal,
            "read artifact".to_string(),
            "done".to_string(),
            round,
        );
        let before = tree_snapshot(&session_root);
        assert!(matches!(
            activation.save_canonical_operation(
                revision("rev-b"),
                "2026-08-20T10:03:00.000Z".to_string(),
                input,
            ),
            Err(SessionMountSaveErrorV1::Canonical(
                CanonicalMountSaveErrorV1::Projection(
                    super::super::canonical::CanonicalResumeProjectionErrorV1::ArtifactClosureRequired {
                        artifact_count: 1
                    }
                )
            ))
        ));
        assert_eq!(tree_snapshot(&session_root), before);
        assert_eq!(activation.saved().updated_at, "2026-08-20T10:02:00.000Z");
    }

    #[test]
    fn canonical_activation_refuses_stale_head_without_allocating_a_candidate() {
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
        let head_path =
            super::super::physical::PhysicalLayoutV1::head_pointer(&session_root, &session);
        let head_a = std::fs::read(&head_path).unwrap();
        let (_, base) =
            super::super::transaction::lookup_canonical_mount_value_v1(&store, &session);
        super::super::transaction::publish_mount_successor_fixture_v1(
            &store,
            &base.unwrap(),
            revision("rev-b"),
            "2026-08-20T10:03:00.000Z".to_string(),
        )
        .unwrap();
        let head_b = std::fs::read(&head_path).unwrap();
        drop(store);

        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();
        let selection = select_session_mount_v1(&directory, store_id.clone(), &session);
        let mut activation =
            activate_session_mount_v1(&directory, store_id, &session, selection).unwrap();
        // Model a non-cooperating writer or external restore bypassing the held
        // fence. The transaction must still compare the exact base before it
        // allocates rev-c.
        std::fs::write(&head_path, &head_a).unwrap();
        let before = tree_snapshot(&session_root);
        let terminal = kitsu_core::RunSpec::new(kitsu_core::RunId::new("run-stale-save").unwrap())
            .terminal(
                kitsu_core::RunOutcomeClass::Succeeded,
                kitsu_core::RunAssistantDisposition::Committed,
            );
        let input = super::super::canonical::CanonicalOperationInputV1::artifact_free_text(
            TurnId::new("turn-stale").unwrap(),
            terminal,
            "stale input".to_string(),
            Some("must not publish".to_string()),
        );
        let error = activation
            .save_canonical_operation(
                revision("rev-c"),
                "2026-08-20T10:04:00.000Z".to_string(),
                input,
            )
            .unwrap_err();
        let SessionMountSaveErrorV1::Canonical(CanonicalMountSaveErrorV1::Transaction(
            transaction_error,
        )) = error
        else {
            panic!("stale HEAD must be a canonical transaction refusal");
        };
        assert!(transaction_error.to_string().contains("revision conflict"));
        assert_eq!(tree_snapshot(&session_root), before);
        assert_eq!(activation.saved().updated_at, "2026-08-20T10:03:00.000Z");

        // A failed attempt does not advance the retained base. Restoring its
        // exact rev-b HEAD permits the same rev-c successor to publish.
        std::fs::write(&head_path, head_b).unwrap();
        let terminal =
            kitsu_core::RunSpec::new(kitsu_core::RunId::new("run-stale-save-retry").unwrap())
                .terminal(
                    kitsu_core::RunOutcomeClass::Succeeded,
                    kitsu_core::RunAssistantDisposition::Committed,
                );
        let input = super::super::canonical::CanonicalOperationInputV1::artifact_free_text(
            TurnId::new("turn-retry").unwrap(),
            terminal,
            "retry input".to_string(),
            Some("publish now".to_string()),
        );
        activation
            .save_canonical_operation(
                revision("rev-c"),
                "2026-08-20T10:04:00.000Z".to_string(),
                input,
            )
            .unwrap();
        assert_eq!(activation.saved().updated_at, "2026-08-20T10:04:00.000Z");
    }
}
