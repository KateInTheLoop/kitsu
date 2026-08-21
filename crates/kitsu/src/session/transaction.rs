//! Provider-free composition of canonical bytes and physical publication.

use std::collections::HashSet;
use std::fmt;

#[cfg(test)]
use std::cell::Cell;

use kitsu_core::session::{
    CommitDurability, CorruptSessionReason, ExpectedRevision, RevisionId, SchemaDimension,
    SessionCommitOutcome, SessionCommitReceipt, SessionId, SessionLookupState, TurnId,
};

#[cfg(test)]
use super::canonical::CanonicalToolRoundV1;
use super::canonical::{
    append_complete_operation, assemble_branch_at_committed_turn, assemble_recovery_revision,
    decode_canonical_payload_classified, encode_canonical_payload,
    prepare_current_v1_legacy_baseline, CanonicalOperationInputV1, CanonicalPayloadDecodeErrorV1,
    CanonicalRecoverySkipV1, CanonicalSessionRevisionV1, ImmutableArtifactRefV1,
    LOGICAL_SCHEMA_REVISION,
};
use super::physical::{
    ArtifactManifestV1, PhysicalArtifactReadErrorV1, PhysicalCandidateInstallError,
    PhysicalFileStoreV1, PhysicalHeadCasError, PhysicalHeadReadErrorV1, PhysicalReadErrorV1,
    VerifiedRevisionCandidateV1,
};

const MAX_CANONICAL_LINEAGE_REVISIONS: usize = 4_096;

fn artifact_fact_matches_physical(
    fact: &ImmutableArtifactRefV1,
    manifest: &ArtifactManifestV1,
) -> bool {
    manifest.artifact_schema_revision() == fact.artifact_schema()
        && manifest.artifact_id() == fact.artifact_id()
        && manifest.content_bytes() == fact.bytes()
        && manifest.content_sha256().as_str() == fact.sha256()
        && manifest.media_type() == fact.media_type()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CandidateDurabilityV1 {
    Confirmed,
    Unconfirmed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CandidateResidueV1 {
    None,
    InstalledUnselected {
        revision: RevisionId,
        durability: CandidateDurabilityV1,
    },
    PreexistingCollision {
        revision: RevisionId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ArtifactResidueV1 {
    Installed {
        artifact_id: String,
        durability: CandidateDurabilityV1,
    },
}

#[derive(Debug)]
struct CanonicalTransactionErrorV1 {
    message: String,
    candidate_residue: CandidateResidueV1,
    artifact_residue: Vec<ArtifactResidueV1>,
    not_published: Option<Box<SessionCommitOutcome>>,
}

impl CanonicalTransactionErrorV1 {
    fn before_identity(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            candidate_residue: CandidateResidueV1::None,
            artifact_residue: Vec::new(),
            not_published: None,
        }
    }

    fn not_published(
        message: impl Into<String>,
        store: &PhysicalFileStoreV1,
        session: &SessionId,
        base_revision: &ExpectedRevision,
        candidate_residue: CandidateResidueV1,
    ) -> Self {
        Self {
            message: message.into(),
            candidate_residue,
            artifact_residue: Vec::new(),
            not_published: Some(Box::new(SessionCommitOutcome::NotPublished {
                store: store.store_id().clone(),
                session: session.clone(),
                base_revision: base_revision.clone(),
            })),
        }
    }

    fn candidate_residue(&self) -> CandidateResidueV1 {
        self.candidate_residue.clone()
    }

    fn with_artifact_residue(mut self, artifact_residue: Vec<ArtifactResidueV1>) -> Self {
        self.artifact_residue = artifact_residue;
        self
    }

    #[cfg(test)]
    fn artifact_residue(&self) -> &[ArtifactResidueV1] {
        &self.artifact_residue
    }

    #[allow(dead_code)]
    fn not_published_outcome(&self) -> Option<&SessionCommitOutcome> {
        self.not_published.as_deref()
    }
}

impl fmt::Display for CanonicalTransactionErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CanonicalTransactionErrorV1 {}

#[derive(Debug)]
enum CanonicalHeadReadErrorV1 {
    Physical {
        observed_revision: Option<RevisionId>,
        reason: CorruptSessionReason,
        message: String,
    },
    Logical {
        observed_revision: RevisionId,
        reason: CorruptSessionReason,
        message: String,
    },
    Binding {
        observed_revision: RevisionId,
        message: String,
    },
    Artifact {
        observed_revision: RevisionId,
        artifact_id: String,
        reason: CorruptSessionReason,
        message: String,
    },
    Unsupported {
        observed_revision: Option<RevisionId>,
        dimension: SchemaDimension,
        observed_schema: u32,
        supported_schema: u32,
        message: String,
    },
}

impl CanonicalHeadReadErrorV1 {
    fn from_physical(error: PhysicalHeadReadErrorV1) -> Self {
        let (observed_revision, source) = error.into_parts();
        Self::from_physical_parts(observed_revision, source)
    }

    fn from_physical_revision(observed_revision: RevisionId, source: PhysicalReadErrorV1) -> Self {
        Self::from_physical_parts(Some(observed_revision), source)
    }

    fn from_physical_parts(
        observed_revision: Option<RevisionId>,
        source: PhysicalReadErrorV1,
    ) -> Self {
        match source {
            PhysicalReadErrorV1::MissingReference(message) => Self::Physical {
                observed_revision,
                reason: CorruptSessionReason::MissingReference,
                message,
            },
            PhysicalReadErrorV1::Malformed(message) => Self::Physical {
                observed_revision,
                reason: CorruptSessionReason::Malformed,
                message,
            },
            PhysicalReadErrorV1::IdentityMismatch(message) => Self::Physical {
                observed_revision,
                reason: CorruptSessionReason::IdentityMismatch,
                message,
            },
            PhysicalReadErrorV1::DigestMismatch(message) => Self::Physical {
                observed_revision,
                reason: CorruptSessionReason::DigestMismatch,
                message,
            },
            PhysicalReadErrorV1::UnsupportedPhysicalSchema {
                observed,
                supported,
                message,
            } => Self::Unsupported {
                observed_revision,
                dimension: SchemaDimension::Physical,
                observed_schema: observed,
                supported_schema: supported,
                message,
            },
        }
    }

    fn observed_revision(&self) -> Option<RevisionId> {
        match self {
            Self::Physical {
                observed_revision, ..
            }
            | Self::Unsupported {
                observed_revision, ..
            } => observed_revision.clone(),
            Self::Logical {
                observed_revision, ..
            }
            | Self::Binding {
                observed_revision, ..
            }
            | Self::Artifact {
                observed_revision, ..
            } => Some(observed_revision.clone()),
        }
    }

    fn lookup_state(&self, session_id: &SessionId) -> SessionLookupState {
        match self {
            Self::Unsupported {
                dimension,
                observed_schema,
                supported_schema,
                ..
            } => SessionLookupState::unsupported(*dimension, *observed_schema, *supported_schema)
                .expect("typed readers never report an equal or zero unsupported schema"),
            _ => {
                let reason = match self {
                    Self::Physical { reason, .. } => *reason,
                    Self::Logical { reason, .. } => *reason,
                    Self::Binding { .. } => CorruptSessionReason::IdentityMismatch,
                    Self::Artifact { reason, .. } => *reason,
                    Self::Unsupported { .. } => unreachable!(),
                };
                SessionLookupState::Corrupt {
                    session: session_id.clone(),
                    observed_revision: self.observed_revision(),
                    reason,
                }
            }
        }
    }
}

impl fmt::Display for CanonicalHeadReadErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Physical { message, .. } => {
                write!(formatter, "physical canonical HEAD is corrupt: {message}")
            }
            Self::Logical { message, .. } => {
                write!(formatter, "canonical HEAD payload is corrupt: {message}")
            }
            Self::Binding { message, .. } => {
                write!(formatter, "canonical HEAD binding is corrupt: {message}")
            }
            Self::Artifact {
                artifact_id,
                message,
                ..
            } => write!(
                formatter,
                "canonical HEAD artifact closure '{artifact_id}' is corrupt: {message}"
            ),
            Self::Unsupported { message, .. } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for CanonicalHeadReadErrorV1 {}

#[derive(Debug)]
struct CanonicalHeadLookupV1 {
    state: SessionLookupState,
    payload: Option<CanonicalSessionRevisionV1>,
}

impl CanonicalHeadLookupV1 {
    fn state(&self) -> &SessionLookupState {
        &self.state
    }

    fn payload(&self) -> Option<&CanonicalSessionRevisionV1> {
        self.payload.as_ref()
    }
}

#[derive(Debug)]
enum CanonicalRecoveryPlanV1 {
    Missing,
    NotNeeded {
        selected: RevisionId,
        head: kitsu_core::session::LogicalHead,
    },
    Available {
        selected: RevisionId,
        selected_manifest_sha256: String,
        selected_state: SessionLookupState,
        recovery: RevisionId,
        recovery_head: kitsu_core::session::LogicalHead,
        recovery_manifest_sha256: String,
        skipped: Vec<CanonicalRecoverySkipV1>,
    },
    Unavailable {
        selected: Option<RevisionId>,
        selected_state: SessionLookupState,
        skipped: Vec<CanonicalRecoverySkipV1>,
        blocker: CanonicalRecoveryBlockerV1,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum CanonicalRecoveryBlockerV1 {
    SelectedState,
    ExhaustedAtRoot {
        revision: RevisionId,
    },
    MissingParentDigest {
        child: RevisionId,
    },
    AncestorState {
        revision: RevisionId,
        state: SessionLookupState,
    },
    ManifestDigestMismatch {
        revision: RevisionId,
    },
    UnsupportedLogicalManifest {
        revision: RevisionId,
        observed: u32,
        supported: u32,
    },
    Cycle {
        revision: RevisionId,
    },
    Limit {
        max_revisions: usize,
    },
}

#[derive(Debug)]
struct VerifiedCanonicalRevisionV1 {
    payload: CanonicalSessionRevisionV1,
    manifest_sha256: String,
    parent_manifest_sha256: Option<String>,
}

#[derive(Debug)]
struct CanonicalRevisionLinkV1 {
    revision: RevisionId,
    parent: ExpectedRevision,
    head: kitsu_core::session::LogicalHead,
    manifest_sha256: String,
    parent_manifest_sha256: Option<String>,
}

impl CanonicalRevisionLinkV1 {
    fn from_verified(verified: VerifiedCanonicalRevisionV1) -> Self {
        Self {
            revision: verified.payload.revision_id().clone(),
            parent: verified.payload.parent_revision().clone(),
            head: verified.payload.selected_head().clone(),
            manifest_sha256: verified.manifest_sha256,
            parent_manifest_sha256: verified.parent_manifest_sha256,
        }
    }

    fn revision_id(&self) -> &RevisionId {
        &self.revision
    }
}

#[derive(Debug)]
struct VerifiedCanonicalLineageV1 {
    revisions: Vec<CanonicalRevisionLinkV1>,
}

impl VerifiedCanonicalLineageV1 {
    fn revisions(&self) -> &[CanonicalRevisionLinkV1] {
        &self.revisions
    }
}

#[derive(Debug)]
struct CanonicalLineageLookupV1 {
    state: SessionLookupState,
    lineage: Option<VerifiedCanonicalLineageV1>,
}

impl CanonicalLineageLookupV1 {
    fn state(&self) -> &SessionLookupState {
        &self.state
    }

    fn lineage(&self) -> Option<&VerifiedCanonicalLineageV1> {
        self.lineage.as_ref()
    }
}

#[derive(Debug)]
struct CanonicalTransactionCommitV1 {
    outcome: SessionCommitOutcome,
    durability_warning: Option<String>,
}

impl CanonicalTransactionCommitV1 {
    fn outcome(&self) -> &SessionCommitOutcome {
        &self.outcome
    }

    fn durability_warning(&self) -> Option<&str> {
        self.durability_warning.as_deref()
    }
}

struct CanonicalFileTransactionV1<'a> {
    store: &'a PhysicalFileStoreV1,
    #[cfg(test)]
    fail_next_after_candidate: Cell<bool>,
    #[cfg(test)]
    fail_next_artifact_recheck: Cell<bool>,
    #[cfg(test)]
    fail_next_after_branch_artifacts: Cell<bool>,
}

impl<'a> CanonicalFileTransactionV1<'a> {
    fn new(store: &'a PhysicalFileStoreV1) -> Self {
        Self {
            store,
            #[cfg(test)]
            fail_next_after_candidate: Cell::new(false),
            #[cfg(test)]
            fail_next_artifact_recheck: Cell::new(false),
            #[cfg(test)]
            fail_next_after_branch_artifacts: Cell::new(false),
        }
    }

    fn commit_current_v1_legacy_baseline(
        &self,
        source_bytes: &[u8],
        expected_session_id: &str,
        revision_id: RevisionId,
        committed_at: String,
    ) -> Result<CanonicalTransactionCommitV1, CanonicalTransactionErrorV1> {
        // Preparation is pure and strict. It always produces a root candidate
        // with ExpectedRevision::Missing, so migration cannot replace an
        // already-published canonical HEAD. The caller-owned v1 bytes are
        // borrowed and remain the rollback source at every failure cut.
        let payload = prepare_current_v1_legacy_baseline(
            source_bytes,
            expected_session_id,
            self.store.store_id().clone(),
            revision_id,
            committed_at,
        )
        .map_err(CanonicalTransactionErrorV1::before_identity)?;
        self.commit(&payload)
    }

    fn open_canonical_head(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<CanonicalSessionRevisionV1>, CanonicalHeadReadErrorV1> {
        let Some(head) = self
            .store
            .read_head(session_id)
            .map_err(CanonicalHeadReadErrorV1::from_physical)?
        else {
            return Ok(None);
        };
        let observed_revision = head.revision_id().clone();
        self.decode_canonical_candidate(session_id, observed_revision, head.candidate())
            .map(|verified| Some(verified.payload))
    }

    fn decode_canonical_candidate(
        &self,
        session_id: &SessionId,
        observed_revision: RevisionId,
        candidate: &VerifiedRevisionCandidateV1,
    ) -> Result<VerifiedCanonicalRevisionV1, CanonicalHeadReadErrorV1> {
        let manifest = candidate.manifest();
        let payload =
            decode_canonical_payload_classified(candidate.canonical_bytes()).map_err(|error| {
                match error {
                    CanonicalPayloadDecodeErrorV1::UnsupportedLogicalSchema {
                        observed,
                        supported,
                    } => CanonicalHeadReadErrorV1::Unsupported {
                        observed_revision: Some(observed_revision.clone()),
                        dimension: SchemaDimension::Logical,
                        observed_schema: observed,
                        supported_schema: supported,
                        message: format!(
                            "unsupported canonical logical schema {observed}; expected {supported}"
                        ),
                    },
                    CanonicalPayloadDecodeErrorV1::Malformed(message) => {
                        CanonicalHeadReadErrorV1::Logical {
                            observed_revision: observed_revision.clone(),
                            reason: CorruptSessionReason::Malformed,
                            message,
                        }
                    }
                    CanonicalPayloadDecodeErrorV1::InvalidHead(message) => {
                        CanonicalHeadReadErrorV1::Logical {
                            observed_revision: observed_revision.clone(),
                            reason: CorruptSessionReason::InvalidHead,
                            message,
                        }
                    }
                    CanonicalPayloadDecodeErrorV1::Invalid(message) => {
                        CanonicalHeadReadErrorV1::Logical {
                            observed_revision: observed_revision.clone(),
                            reason: CorruptSessionReason::BrokenCausality,
                            message,
                        }
                    }
                }
            })?;

        if manifest.logical_schema_revision() != payload.logical_schema()
            || manifest.store_id() != self.store.store_id()
            || payload.store_id() != self.store.store_id()
            || manifest.session_id() != session_id
            || payload.session_id() != session_id
            || manifest.revision_id() != &observed_revision
            || payload.revision_id() != &observed_revision
            || manifest.parent_revision() != payload.parent_revision()
        {
            return Err(CanonicalHeadReadErrorV1::Binding {
                observed_revision,
                message: "logical payload identity, schema, or parent does not match the selected physical revision"
                    .to_string(),
            });
        }

        self.verify_artifact_closure_for_read(&payload)?;
        Ok(VerifiedCanonicalRevisionV1 {
            payload,
            manifest_sha256: candidate.manifest_sha256().as_str().to_string(),
            parent_manifest_sha256: manifest
                .parent_manifest_sha256()
                .map(|digest| digest.as_str().to_string()),
        })
    }

    fn lookup_canonical_head(&self, session_id: &SessionId) -> CanonicalHeadLookupV1 {
        match self.open_canonical_head(session_id) {
            Ok(None) => CanonicalHeadLookupV1 {
                state: SessionLookupState::Missing {
                    tombstone_revision: None,
                },
                payload: None,
            },
            Ok(Some(payload)) => CanonicalHeadLookupV1 {
                state: SessionLookupState::Present {
                    revision: payload.revision_id().clone(),
                    head: payload.selected_head().clone(),
                },
                payload: Some(payload),
            },
            Err(error) => CanonicalHeadLookupV1 {
                state: error.lookup_state(session_id),
                payload: None,
            },
        }
    }

    fn plan_last_good_recovery(&self, session_id: &SessionId) -> CanonicalRecoveryPlanV1 {
        let selected_head = match self.store.read_head_envelope(session_id) {
            Ok(None) => return CanonicalRecoveryPlanV1::Missing,
            Ok(Some(head)) => head,
            Err(error) => {
                let error = CanonicalHeadReadErrorV1::from_physical(error);
                return CanonicalRecoveryPlanV1::Unavailable {
                    selected: error.observed_revision(),
                    selected_state: error.lookup_state(session_id),
                    skipped: Vec::new(),
                    blocker: CanonicalRecoveryBlockerV1::SelectedState,
                };
            }
        };
        let selected = selected_head.revision_id().clone();
        let selected_manifest_sha256 = selected_head
            .envelope()
            .manifest_sha256()
            .as_str()
            .to_string();
        let selected_error = match self
            .store
            .verify_revision_candidate(selected_head.envelope())
        {
            Ok(candidate) => {
                match self.decode_canonical_candidate(session_id, selected.clone(), &candidate) {
                    Ok(verified) => {
                        return CanonicalRecoveryPlanV1::NotNeeded {
                            selected,
                            head: verified.payload.selected_head().clone(),
                        };
                    }
                    Err(error) => error,
                }
            }
            Err(error) => CanonicalHeadReadErrorV1::from_physical_revision(selected.clone(), error),
        };
        let selected_state = selected_error.lookup_state(session_id);
        let mut skipped = Vec::new();
        if let SessionLookupState::Corrupt {
            observed_revision: Some(revision),
            reason,
            ..
        } = &selected_state
        {
            skipped.push(CanonicalRecoverySkipV1::new(revision.clone(), *reason));
        }
        if matches!(selected_state, SessionLookupState::Unsupported { .. })
            || selected_head.manifest().logical_schema_revision() != LOGICAL_SCHEMA_REVISION
        {
            return CanonicalRecoveryPlanV1::Unavailable {
                selected: Some(selected),
                selected_state,
                skipped,
                blocker: CanonicalRecoveryBlockerV1::SelectedState,
            };
        }

        // Recovery is advisory and read-only. It follows only the exact,
        // digest-bound physical ancestry of the selected corrupt revision;
        // unselected candidates and directory order can never influence it.
        let mut parent = selected_head.manifest().parent_revision().clone();
        let mut parent_manifest_sha256 = selected_head
            .manifest()
            .parent_manifest_sha256()
            .map(|digest| digest.as_str().to_string());
        let mut seen = HashSet::new();
        seen.insert(selected.clone());
        let mut frontier = selected.clone();

        loop {
            let ExpectedRevision::Exact(parent_revision) = parent else {
                return CanonicalRecoveryPlanV1::Unavailable {
                    selected: Some(selected),
                    selected_state,
                    skipped,
                    blocker: CanonicalRecoveryBlockerV1::ExhaustedAtRoot { revision: frontier },
                };
            };
            if seen.len() >= MAX_CANONICAL_LINEAGE_REVISIONS {
                return CanonicalRecoveryPlanV1::Unavailable {
                    selected: Some(selected),
                    selected_state,
                    skipped,
                    blocker: CanonicalRecoveryBlockerV1::Limit {
                        max_revisions: MAX_CANONICAL_LINEAGE_REVISIONS,
                    },
                };
            }
            if !seen.insert(parent_revision.clone()) {
                return CanonicalRecoveryPlanV1::Unavailable {
                    selected: Some(selected),
                    selected_state,
                    skipped,
                    blocker: CanonicalRecoveryBlockerV1::Cycle {
                        revision: parent_revision,
                    },
                };
            }
            let Some(expected_manifest_sha256) = parent_manifest_sha256.as_deref() else {
                return CanonicalRecoveryPlanV1::Unavailable {
                    selected: Some(selected),
                    selected_state,
                    skipped,
                    blocker: CanonicalRecoveryBlockerV1::MissingParentDigest { child: frontier },
                };
            };
            let envelope = match self
                .store
                .open_revision_envelope(session_id, &parent_revision)
            {
                Ok(envelope) => envelope,
                Err(error) => {
                    let state = CanonicalHeadReadErrorV1::from_physical_revision(
                        parent_revision.clone(),
                        error,
                    )
                    .lookup_state(session_id);
                    return CanonicalRecoveryPlanV1::Unavailable {
                        selected: Some(selected),
                        selected_state,
                        skipped,
                        blocker: CanonicalRecoveryBlockerV1::AncestorState {
                            revision: parent_revision,
                            state,
                        },
                    };
                }
            };
            if envelope.manifest_sha256().as_str() != expected_manifest_sha256 {
                return CanonicalRecoveryPlanV1::Unavailable {
                    selected: Some(selected),
                    selected_state,
                    skipped,
                    blocker: CanonicalRecoveryBlockerV1::ManifestDigestMismatch {
                        revision: parent_revision,
                    },
                };
            }
            let observed_schema = envelope.manifest().logical_schema_revision();
            if observed_schema != LOGICAL_SCHEMA_REVISION {
                return CanonicalRecoveryPlanV1::Unavailable {
                    selected: Some(selected),
                    selected_state,
                    skipped,
                    blocker: CanonicalRecoveryBlockerV1::UnsupportedLogicalManifest {
                        revision: parent_revision,
                        observed: observed_schema,
                        supported: LOGICAL_SCHEMA_REVISION,
                    },
                };
            }

            let candidate = match self.store.verify_revision_candidate(&envelope) {
                Ok(candidate) => candidate,
                Err(error) => {
                    let state = CanonicalHeadReadErrorV1::from_physical_revision(
                        parent_revision.clone(),
                        error,
                    )
                    .lookup_state(session_id);
                    if let SessionLookupState::Corrupt { reason, .. } = state {
                        skipped.push(CanonicalRecoverySkipV1::new(
                            parent_revision.clone(),
                            reason,
                        ));
                    }
                    frontier = parent_revision;
                    parent = envelope.manifest().parent_revision().clone();
                    parent_manifest_sha256 = envelope
                        .manifest()
                        .parent_manifest_sha256()
                        .map(|digest| digest.as_str().to_string());
                    continue;
                }
            };
            match self.decode_canonical_candidate(session_id, parent_revision.clone(), &candidate) {
                Ok(verified) => {
                    let recovery_head = verified.payload.selected_head().clone();
                    return CanonicalRecoveryPlanV1::Available {
                        selected,
                        selected_manifest_sha256,
                        selected_state,
                        recovery: parent_revision,
                        recovery_head,
                        recovery_manifest_sha256: verified.manifest_sha256,
                        skipped,
                    };
                }
                Err(error) => {
                    let state = error.lookup_state(session_id);
                    if matches!(state, SessionLookupState::Unsupported { .. }) {
                        return CanonicalRecoveryPlanV1::Unavailable {
                            selected: Some(selected),
                            selected_state,
                            skipped,
                            blocker: CanonicalRecoveryBlockerV1::AncestorState {
                                revision: parent_revision,
                                state,
                            },
                        };
                    }
                    let SessionLookupState::Corrupt { reason, .. } = state else {
                        unreachable!("canonical candidate errors are corrupt or unsupported")
                    };
                    skipped.push(CanonicalRecoverySkipV1::new(
                        parent_revision.clone(),
                        reason,
                    ));
                }
            }
            frontier = parent_revision;
            parent = envelope.manifest().parent_revision().clone();
            parent_manifest_sha256 = envelope
                .manifest()
                .parent_manifest_sha256()
                .map(|digest| digest.as_str().to_string());
        }
    }

    fn lookup_canonical_lineage(&self, session_id: &SessionId) -> CanonicalLineageLookupV1 {
        self.lookup_canonical_lineage_bounded(session_id, MAX_CANONICAL_LINEAGE_REVISIONS)
    }

    fn lookup_canonical_lineage_bounded(
        &self,
        session_id: &SessionId,
        max_revisions: usize,
    ) -> CanonicalLineageLookupV1 {
        assert!(max_revisions > 0, "canonical lineage bound must be nonzero");
        let selected_head = match self.store.read_head(session_id) {
            Ok(None) => {
                return CanonicalLineageLookupV1 {
                    state: SessionLookupState::Missing {
                        tombstone_revision: None,
                    },
                    lineage: None,
                };
            }
            Ok(Some(head)) => head,
            Err(error) => {
                let error = CanonicalHeadReadErrorV1::from_physical(error);
                return CanonicalLineageLookupV1 {
                    state: error.lookup_state(session_id),
                    lineage: None,
                };
            }
        };
        let selected_revision = selected_head.revision_id().clone();
        let selected = match self.decode_canonical_candidate(
            session_id,
            selected_revision,
            selected_head.candidate(),
        ) {
            Ok(verified) => verified,
            Err(error) => {
                return CanonicalLineageLookupV1 {
                    state: error.lookup_state(session_id),
                    lineage: None,
                };
            }
        };

        let selected_link = CanonicalRevisionLinkV1::from_verified(selected);
        let present = SessionLookupState::Present {
            revision: selected_link.revision.clone(),
            head: selected_link.head.clone(),
        };
        let mut seen = HashSet::new();
        seen.insert(selected_link.revision.clone());
        let mut revisions = vec![selected_link];

        loop {
            let parent = revisions
                .last()
                .expect("selected canonical lineage always has one revision")
                .parent
                .clone();
            let ExpectedRevision::Exact(parent_revision) = parent else {
                return CanonicalLineageLookupV1 {
                    state: present,
                    lineage: Some(VerifiedCanonicalLineageV1 { revisions }),
                };
            };
            if revisions.len() >= max_revisions || !seen.insert(parent_revision.clone()) {
                return CanonicalLineageLookupV1 {
                    state: SessionLookupState::Corrupt {
                        session: session_id.clone(),
                        observed_revision: Some(parent_revision),
                        reason: CorruptSessionReason::BrokenCausality,
                    },
                    lineage: None,
                };
            }

            let candidate = match self
                .store
                .open_revision_candidate(session_id, &parent_revision)
            {
                Ok(candidate) => candidate,
                Err(error) => {
                    let error =
                        CanonicalHeadReadErrorV1::from_physical_revision(parent_revision, error);
                    return CanonicalLineageLookupV1 {
                        state: error.lookup_state(session_id),
                        lineage: None,
                    };
                }
            };
            let expected_parent_manifest = revisions
                .last()
                .expect("selected canonical lineage always has one revision")
                .parent_manifest_sha256
                .as_deref()
                .expect("exact parent revision always binds its manifest digest");
            if candidate.manifest_sha256().as_str() != expected_parent_manifest {
                return CanonicalLineageLookupV1 {
                    state: SessionLookupState::Corrupt {
                        session: session_id.clone(),
                        observed_revision: Some(parent_revision),
                        reason: CorruptSessionReason::DigestMismatch,
                    },
                    lineage: None,
                };
            }
            let verified =
                match self.decode_canonical_candidate(session_id, parent_revision, &candidate) {
                    Ok(verified) => verified,
                    Err(error) => {
                        return CanonicalLineageLookupV1 {
                            state: error.lookup_state(session_id),
                            lineage: None,
                        };
                    }
                };
            revisions.push(CanonicalRevisionLinkV1::from_verified(verified));
        }
    }

    fn commit_artifact_free_operation(
        &self,
        base: &CanonicalSessionRevisionV1,
        revision_id: RevisionId,
        committed_at: String,
        input: CanonicalOperationInputV1,
    ) -> Result<CanonicalTransactionCommitV1, CanonicalTransactionErrorV1> {
        // This composition deliberately cannot publish new or inherited
        // artifact references until physical artifact installation and closure
        // verification have their own accepted fault matrix.
        if base.has_artifacts() {
            return Err(CanonicalTransactionErrorV1::before_identity(
                "artifact-bearing canonical history requires durable physical artifact closure",
            ));
        }
        let payload = append_complete_operation(base, revision_id, committed_at, input)
            .map_err(CanonicalTransactionErrorV1::before_identity)?;
        self.commit(&payload)
    }

    fn commit_artifact_tool_operation(
        &self,
        base: &CanonicalSessionRevisionV1,
        revision_id: RevisionId,
        committed_at: String,
        input: CanonicalOperationInputV1,
    ) -> Result<CanonicalTransactionCommitV1, CanonicalTransactionErrorV1> {
        let payload = append_complete_operation(base, revision_id, committed_at, input)
            .map_err(CanonicalTransactionErrorV1::before_identity)?;
        if !payload.has_artifacts() {
            return Err(CanonicalTransactionErrorV1::before_identity(
                "artifact tool operation must carry an immutable artifact closure",
            ));
        }
        self.commit(&payload)
    }

    fn commit_branch_at_turn(
        &self,
        source: &CanonicalSessionRevisionV1,
        destination_session: SessionId,
        destination_revision: RevisionId,
        committed_at: String,
        branch_turn: TurnId,
    ) -> Result<CanonicalTransactionCommitV1, CanonicalTransactionErrorV1> {
        if source.store_id() != self.store.store_id() {
            return Err(CanonicalTransactionErrorV1::before_identity(format!(
                "canonical branch source store identity '{}' does not match physical store '{}'",
                source.store_id(),
                self.store.store_id()
            )));
        }
        let payload = assemble_branch_at_committed_turn(
            source,
            self.store.store_id().clone(),
            destination_session.clone(),
            destination_revision.clone(),
            committed_at,
            branch_turn,
        )
        .map_err(CanonicalTransactionErrorV1::before_identity)?;
        let canonical_bytes = encode_canonical_payload(&payload)
            .map_err(CanonicalTransactionErrorV1::before_identity)?;
        let expected = ExpectedRevision::Missing;
        let confirmed_receipt = self.receipt(&payload, CommitDurability::Confirmed)?;
        let unconfirmed_receipt = self.receipt(&payload, CommitDurability::PublishedUnconfirmed)?;

        // Always acquire two session fences in stable identity order. This
        // avoids AB/BA deadlock while retaining both guards until destination
        // HEAD publication settles.
        let acquire = |session: &SessionId| {
            self.store.acquire_writer_fence(session).map_err(|message| {
                CanonicalTransactionErrorV1::not_published(
                    message,
                    self.store,
                    &destination_session,
                    &expected,
                    CandidateResidueV1::None,
                )
            })
        };
        let (_source_fence, destination_fence) =
            if source.session_id().as_str() < destination_session.as_str() {
                let source_fence = acquire(source.session_id())?;
                let destination_fence = acquire(&destination_session)?;
                (source_fence, destination_fence)
            } else {
                let destination_fence = acquire(&destination_session)?;
                let source_fence = acquire(source.session_id())?;
                (source_fence, destination_fence)
            };

        let admitted_source = self
            .open_canonical_head(source.session_id())
            .map_err(|error| {
                CanonicalTransactionErrorV1::not_published(
                    format!("canonical branch source no longer verifies: {error}"),
                    self.store,
                    &destination_session,
                    &expected,
                    CandidateResidueV1::None,
                )
            })?
            .ok_or_else(|| {
                CanonicalTransactionErrorV1::not_published(
                    "canonical branch source changed: selected HEAD disappeared",
                    self.store,
                    &destination_session,
                    &expected,
                    CandidateResidueV1::None,
                )
            })?;
        if &admitted_source != source {
            return Err(CanonicalTransactionErrorV1::not_published(
                "canonical branch source changed before fenced admission",
                self.store,
                &destination_session,
                &expected,
                CandidateResidueV1::None,
            ));
        }

        let destination_head = self
            .store
            .read_head(&destination_session)
            .map_err(|error| {
                CanonicalTransactionErrorV1::not_published(
                    format!("cannot prove branch destination HEAD absent: {error}"),
                    self.store,
                    &destination_session,
                    &expected,
                    CandidateResidueV1::None,
                )
            })?;
        if destination_head.is_some() {
            return Err(CanonicalTransactionErrorV1::not_published(
                "canonical branch destination HEAD already exists; refusing to overwrite it",
                self.store,
                &destination_session,
                &expected,
                CandidateResidueV1::None,
            ));
        }
        let existing_candidate_manifest = if self
            .store
            .revision_candidate_is_absent(&destination_session, &destination_revision)
            .map_err(|error| {
                CanonicalTransactionErrorV1::not_published(
                    format!("cannot prove branch destination revision absent: {error}"),
                    self.store,
                    &destination_session,
                    &expected,
                    CandidateResidueV1::None,
                )
            })? {
            None
        } else {
            let existing = self
                .store
                .open_revision_candidate(&destination_session, &destination_revision)
                .map_err(|error| {
                    CanonicalTransactionErrorV1::not_published(
                        format!(
                            "canonical branch destination revision already exists but cannot be verified: {error}"
                        ),
                        self.store,
                        &destination_session,
                        &expected,
                        CandidateResidueV1::PreexistingCollision {
                            revision: destination_revision.clone(),
                        },
                    )
                })?;
            if existing.manifest().logical_schema_revision() != payload.logical_schema()
                || existing.manifest().parent_revision() != &expected
                || existing.canonical_bytes() != canonical_bytes
            {
                return Err(CanonicalTransactionErrorV1::not_published(
                    "canonical branch destination revision already exists with different bytes; refusing to overwrite it",
                    self.store,
                    &destination_session,
                    &expected,
                    CandidateResidueV1::PreexistingCollision {
                        revision: destination_revision.clone(),
                    },
                ));
            }
            Some(existing)
        };

        // Preflight the complete destination artifact set before copying the
        // first byte. The destination fence excludes every conformant writer.
        let mut existing_artifact_manifests = Vec::with_capacity(payload.artifact_count());
        for fact in payload.artifacts() {
            match self
                .store
                .open_artifact(&destination_session, fact.artifact_id())
            {
                Err(PhysicalArtifactReadErrorV1::Missing(_)) => {
                    existing_artifact_manifests.push(None)
                }
                Ok(existing) if artifact_fact_matches_physical(fact, existing.manifest()) => {
                    existing_artifact_manifests
                        .push(Some(existing.manifest_sha256().as_str().to_string()));
                }
                Ok(_) => {
                    return Err(CanonicalTransactionErrorV1::not_published(
                        format!(
                            "canonical branch destination artifact '{}' already exists with different bytes; refusing to overwrite it",
                            fact.artifact_id()
                        ),
                        self.store,
                        &destination_session,
                        &expected,
                        CandidateResidueV1::None,
                    ));
                }
                Err(error) => {
                    return Err(CanonicalTransactionErrorV1::not_published(
                        format!(
                            "canonical branch destination artifact '{}' already exists but cannot be verified: {error}",
                            fact.artifact_id()
                        ),
                        self.store,
                        &destination_session,
                        &expected,
                        CandidateResidueV1::None,
                    ));
                }
            }
        }

        let mut artifact_residue = Vec::with_capacity(payload.artifact_count());
        for (fact, existing_manifest) in payload.artifacts().iter().zip(existing_artifact_manifests)
        {
            if let Some(existing_manifest) = existing_manifest {
                let confirmed = self
                    .store
                    .confirm_existing_artifact(
                        &destination_fence,
                        &destination_session,
                        fact.artifact_id(),
                        &existing_manifest,
                    )
                    .map_err(|error| {
                        CanonicalTransactionErrorV1::not_published(
                            format!(
                                "cannot confirm canonical branch destination artifact '{}': {error}",
                                fact.artifact_id()
                            ),
                            self.store,
                            &destination_session,
                            &expected,
                            CandidateResidueV1::None,
                        )
                        .with_artifact_residue(artifact_residue.clone())
                    })?;
                if !artifact_fact_matches_physical(fact, confirmed.manifest()) {
                    return Err(CanonicalTransactionErrorV1::not_published(
                        format!(
                            "canonical branch destination artifact '{}' changed during exact adoption",
                            fact.artifact_id()
                        ),
                        self.store,
                        &destination_session,
                        &expected,
                        CandidateResidueV1::None,
                    )
                    .with_artifact_residue(artifact_residue));
                }
                artifact_residue.push(ArtifactResidueV1::Installed {
                    artifact_id: fact.artifact_id().to_string(),
                    durability: CandidateDurabilityV1::Confirmed,
                });
            } else {
                match self.store.copy_artifact_no_overwrite(
                    source.session_id(),
                    &destination_session,
                    fact.artifact_id(),
                ) {
                    Ok(_) => artifact_residue.push(ArtifactResidueV1::Installed {
                        artifact_id: fact.artifact_id().to_string(),
                        durability: CandidateDurabilityV1::Confirmed,
                    }),
                    Err(error) => {
                        if error.installed() {
                            artifact_residue.push(ArtifactResidueV1::Installed {
                                artifact_id: fact.artifact_id().to_string(),
                                durability: CandidateDurabilityV1::Unconfirmed,
                            });
                        }
                        return Err(CanonicalTransactionErrorV1::not_published(
                            format!(
                                "cannot copy canonical branch artifact '{}': {error}",
                                fact.artifact_id()
                            ),
                            self.store,
                            &destination_session,
                            &expected,
                            CandidateResidueV1::None,
                        )
                        .with_artifact_residue(artifact_residue));
                    }
                }
            }
        }

        #[cfg(test)]
        if self.fail_next_after_branch_artifacts.replace(false) {
            return Err(CanonicalTransactionErrorV1::not_published(
                "injected failure after canonical branch artifact copying",
                self.store,
                &destination_session,
                &expected,
                CandidateResidueV1::None,
            )
            .with_artifact_residue(artifact_residue));
        }

        self.verify_artifact_closure(&payload, CandidateResidueV1::None)
            .map_err(|error| error.with_artifact_residue(artifact_residue.clone()))?;
        let candidate = if let Some(existing_candidate) = existing_candidate_manifest {
            self.store
                .adopt_revision_candidate_exact(
                    &destination_fence,
                    &existing_candidate,
                    &canonical_bytes,
                )
                .map_err(|error| {
                    CanonicalTransactionErrorV1::not_published(
                        format!(
                            "cannot adopt exact canonical branch destination revision: {error}"
                        ),
                        self.store,
                        &destination_session,
                        &expected,
                        CandidateResidueV1::PreexistingCollision {
                            revision: destination_revision.clone(),
                        },
                    )
                    .with_artifact_residue(artifact_residue.clone())
                })?
        } else {
            self.store
                .install_revision_candidate(
                    payload.logical_schema(),
                    destination_session.clone(),
                    destination_revision.clone(),
                    expected.clone(),
                    &canonical_bytes,
                )
                .map_err(|error| {
                    self.install_error(&payload, &canonical_bytes, error)
                        .with_artifact_residue(artifact_residue.clone())
                })?
        };

        #[cfg(test)]
        if self.fail_next_artifact_recheck.replace(false) {
            return Err(CanonicalTransactionErrorV1::not_published(
                "injected canonical branch artifact closure recheck failure",
                self.store,
                &destination_session,
                &expected,
                CandidateResidueV1::InstalledUnselected {
                    revision: destination_revision.clone(),
                    durability: CandidateDurabilityV1::Confirmed,
                },
            )
            .with_artifact_residue(artifact_residue));
        }

        #[cfg(test)]
        if self.fail_next_after_candidate.replace(false) {
            return Err(CanonicalTransactionErrorV1::not_published(
                "injected failure after immutable branch candidate installation",
                self.store,
                &destination_session,
                &expected,
                CandidateResidueV1::InstalledUnselected {
                    revision: destination_revision.clone(),
                    durability: CandidateDurabilityV1::Confirmed,
                },
            )
            .with_artifact_residue(artifact_residue));
        }

        let source_recheck = self
            .open_canonical_head(source.session_id())
            .map_err(|error| {
                CanonicalTransactionErrorV1::not_published(
                    format!("canonical branch source changed before publication: {error}"),
                    self.store,
                    &destination_session,
                    &expected,
                    CandidateResidueV1::InstalledUnselected {
                        revision: destination_revision.clone(),
                        durability: CandidateDurabilityV1::Confirmed,
                    },
                )
                .with_artifact_residue(artifact_residue.clone())
            })?;
        if source_recheck.as_ref() != Some(source) {
            return Err(CanonicalTransactionErrorV1::not_published(
                "canonical branch source changed before publication",
                self.store,
                &destination_session,
                &expected,
                CandidateResidueV1::InstalledUnselected {
                    revision: destination_revision.clone(),
                    durability: CandidateDurabilityV1::Confirmed,
                },
            )
            .with_artifact_residue(artifact_residue));
        }
        self.verify_artifact_closure(
            &payload,
            CandidateResidueV1::InstalledUnselected {
                revision: destination_revision.clone(),
                durability: CandidateDurabilityV1::Confirmed,
            },
        )
        .map_err(|error| error.with_artifact_residue(artifact_residue.clone()))?;

        let (receipt, durability_warning) =
            match self
                .store
                .compare_and_swap_head(&destination_fence, expected.clone(), &candidate)
            {
                Ok(_) => (confirmed_receipt, None),
                Err(PhysicalHeadCasError::Publication {
                    message,
                    installed: true,
                }) => (unconfirmed_receipt, Some(message)),
                Err(error) => {
                    return Err(CanonicalTransactionErrorV1::not_published(
                        error.to_string(),
                        self.store,
                        &destination_session,
                        &expected,
                        CandidateResidueV1::InstalledUnselected {
                            revision: destination_revision,
                            durability: CandidateDurabilityV1::Confirmed,
                        },
                    )
                    .with_artifact_residue(artifact_residue));
                }
            };
        Ok(CanonicalTransactionCommitV1 {
            outcome: SessionCommitOutcome::Published { receipt },
            durability_warning,
        })
    }

    fn commit_last_good_recovery(
        &self,
        session_id: &SessionId,
        revision_id: RevisionId,
        committed_at: String,
    ) -> Result<CanonicalTransactionCommitV1, CanonicalTransactionErrorV1> {
        // A read-only hint lets a failed fence acquisition still report the
        // exact base it attempted. Admission is repeated under the fence before
        // any candidate can be allocated.
        let selected_hint = self
            .store
            .read_head_envelope(session_id)
            .map_err(|error| CanonicalTransactionErrorV1::before_identity(error.to_string()))?
            .ok_or_else(|| {
                CanonicalTransactionErrorV1::before_identity(
                    "cannot recover a canonical session without a selected HEAD",
                )
            })?;
        let expected = ExpectedRevision::Exact(selected_hint.revision_id().clone());
        let fence = self
            .store
            .acquire_writer_fence(session_id)
            .map_err(|message| {
                CanonicalTransactionErrorV1::not_published(
                    message,
                    self.store,
                    session_id,
                    &expected,
                    CandidateResidueV1::None,
                )
            })?;
        let selected = self
            .store
            .read_head_envelope(session_id)
            .map_err(|error| {
                CanonicalTransactionErrorV1::not_published(
                    error.to_string(),
                    self.store,
                    session_id,
                    &expected,
                    CandidateResidueV1::None,
                )
            })?
            .ok_or_else(|| {
                CanonicalTransactionErrorV1::not_published(
                    "selected canonical HEAD disappeared before recovery admission",
                    self.store,
                    session_id,
                    &expected,
                    CandidateResidueV1::None,
                )
            })?;
        if selected.revision_id() != selected_hint.revision_id()
            || selected.envelope().manifest_sha256() != selected_hint.envelope().manifest_sha256()
        {
            return Err(CanonicalTransactionErrorV1::not_published(
                "selected canonical HEAD changed before recovery admission",
                self.store,
                session_id,
                &expected,
                CandidateResidueV1::None,
            ));
        }

        let (
            selected_revision,
            selected_manifest_sha256,
            source_revision,
            source_head,
            source_manifest_sha256,
            skipped,
        ) = match self.plan_last_good_recovery(session_id) {
            CanonicalRecoveryPlanV1::Available {
                selected,
                selected_manifest_sha256,
                recovery,
                recovery_head,
                recovery_manifest_sha256,
                skipped,
                ..
            } => (
                selected,
                selected_manifest_sha256,
                recovery,
                recovery_head,
                recovery_manifest_sha256,
                skipped,
            ),
            plan => {
                return Err(CanonicalTransactionErrorV1::not_published(
                    format!("canonical recovery is unavailable: {plan:?}"),
                    self.store,
                    session_id,
                    &expected,
                    CandidateResidueV1::None,
                ));
            }
        };
        if selected_revision != *selected.revision_id()
            || selected_manifest_sha256 != selected.envelope().manifest_sha256().as_str()
        {
            return Err(CanonicalTransactionErrorV1::not_published(
                "recovery plan selected a different canonical HEAD envelope",
                self.store,
                session_id,
                &expected,
                CandidateResidueV1::None,
            ));
        }
        let source_candidate = self
            .store
            .open_revision_candidate(session_id, &source_revision)
            .map_err(|error| {
                CanonicalTransactionErrorV1::not_published(
                    format!("recovery source changed after planning: {error}"),
                    self.store,
                    session_id,
                    &expected,
                    CandidateResidueV1::None,
                )
            })?;
        if source_candidate.manifest_sha256().as_str() != source_manifest_sha256 {
            return Err(CanonicalTransactionErrorV1::not_published(
                "recovery source manifest changed after planning",
                self.store,
                session_id,
                &expected,
                CandidateResidueV1::None,
            ));
        }
        let source = self
            .decode_canonical_candidate(session_id, source_revision.clone(), &source_candidate)
            .map_err(|error| {
                CanonicalTransactionErrorV1::not_published(
                    format!("recovery source no longer verifies: {error}"),
                    self.store,
                    session_id,
                    &expected,
                    CandidateResidueV1::None,
                )
            })?
            .payload;
        if source.selected_head() != &source_head {
            return Err(CanonicalTransactionErrorV1::not_published(
                "recovery source HEAD changed after planning",
                self.store,
                session_id,
                &expected,
                CandidateResidueV1::None,
            ));
        }
        let payload = assemble_recovery_revision(
            &source,
            selected_revision,
            revision_id,
            committed_at,
            skipped,
        )
        .map_err(|error| {
            CanonicalTransactionErrorV1::not_published(
                error,
                self.store,
                session_id,
                &expected,
                CandidateResidueV1::None,
            )
        })?;
        let canonical_bytes = encode_canonical_payload(&payload)
            .map_err(CanonicalTransactionErrorV1::before_identity)?;
        let confirmed_receipt = self.receipt(&payload, CommitDurability::Confirmed)?;
        let unconfirmed_receipt = self.receipt(&payload, CommitDurability::PublishedUnconfirmed)?;

        self.verify_artifact_closure(&payload, CandidateResidueV1::None)?;
        let candidate = self
            .store
            .install_revision_candidate_against_envelope(
                payload.logical_schema(),
                session_id.clone(),
                payload.revision_id().clone(),
                selected.envelope(),
                &canonical_bytes,
            )
            .map_err(|error| self.install_error(&payload, &canonical_bytes, error))?;
        #[cfg(test)]
        if self.fail_next_artifact_recheck.replace(false) {
            return Err(CanonicalTransactionErrorV1::not_published(
                "injected recovery artifact closure recheck failure",
                self.store,
                session_id,
                &expected,
                CandidateResidueV1::InstalledUnselected {
                    revision: payload.revision_id().clone(),
                    durability: CandidateDurabilityV1::Confirmed,
                },
            ));
        }
        self.verify_artifact_closure(
            &payload,
            CandidateResidueV1::InstalledUnselected {
                revision: payload.revision_id().clone(),
                durability: CandidateDurabilityV1::Confirmed,
            },
        )?;

        #[cfg(test)]
        if self.fail_next_after_candidate.replace(false) {
            return Err(CanonicalTransactionErrorV1::not_published(
                "injected failure after immutable recovery candidate installation",
                self.store,
                session_id,
                &expected,
                CandidateResidueV1::InstalledUnselected {
                    revision: payload.revision_id().clone(),
                    durability: CandidateDurabilityV1::Confirmed,
                },
            ));
        }

        let (receipt, durability_warning) = match self
            .store
            .compare_and_swap_head_against_envelope(&fence, &selected, &candidate)
        {
            Ok(_) => (confirmed_receipt, None),
            Err(PhysicalHeadCasError::Publication {
                message,
                installed: true,
            }) => (unconfirmed_receipt, Some(message)),
            Err(error) => {
                return Err(CanonicalTransactionErrorV1::not_published(
                    error.to_string(),
                    self.store,
                    session_id,
                    &expected,
                    CandidateResidueV1::InstalledUnselected {
                        revision: payload.revision_id().clone(),
                        durability: CandidateDurabilityV1::Confirmed,
                    },
                ));
            }
        };
        Ok(CanonicalTransactionCommitV1 {
            outcome: SessionCommitOutcome::Published { receipt },
            durability_warning,
        })
    }

    fn commit(
        &self,
        payload: &CanonicalSessionRevisionV1,
    ) -> Result<CanonicalTransactionCommitV1, CanonicalTransactionErrorV1> {
        // Validate and deterministically encode before a lock file, candidate,
        // or HEAD can be touched. The transaction stores these exact bytes.
        let canonical_bytes = encode_canonical_payload(payload)
            .map_err(CanonicalTransactionErrorV1::before_identity)?;
        if payload.store_id() != self.store.store_id() {
            return Err(CanonicalTransactionErrorV1::before_identity(format!(
                "canonical store identity '{}' does not match physical store '{}'",
                payload.store_id(),
                self.store.store_id()
            )));
        }

        let session = payload.session_id();
        let revision = payload.revision_id();
        let expected = payload.parent_revision();
        let confirmed_receipt = self.receipt(payload, CommitDurability::Confirmed)?;
        let unconfirmed_receipt = self.receipt(payload, CommitDurability::PublishedUnconfirmed)?;
        let fence = self
            .store
            .acquire_writer_fence(session)
            .map_err(|message| {
                CanonicalTransactionErrorV1::not_published(
                    message,
                    self.store,
                    session,
                    expected,
                    CandidateResidueV1::None,
                )
            })?;

        // Pre-admission under the exact session fence keeps an already-stale
        // transaction from allocating even an immutable orphan. The physical
        // CAS repeats this comparison immediately before publication.
        let current = self.store.read_head(session).map_err(|message| {
            CanonicalTransactionErrorV1::not_published(
                message.to_string(),
                self.store,
                session,
                expected,
                CandidateResidueV1::None,
            )
        })?;
        let observed = current.as_ref().map(|head| head.revision_id());
        if let Err(state) = expected.admit_observed(observed) {
            let message = match state {
                SessionLookupState::Conflict { expected, observed } => format!(
                    "canonical transaction revision conflict: expected {expected:?}, observed {observed:?}"
                ),
                other => format!("canonical transaction admission returned invalid state: {other:?}"),
            };
            return Err(CanonicalTransactionErrorV1::not_published(
                message,
                self.store,
                session,
                expected,
                CandidateResidueV1::None,
            ));
        }

        self.verify_artifact_closure(payload, CandidateResidueV1::None)?;

        let candidate = self
            .store
            .install_revision_candidate(
                payload.logical_schema(),
                session.clone(),
                revision.clone(),
                expected.clone(),
                &canonical_bytes,
            )
            .map_err(|error| self.install_error(payload, &canonical_bytes, error))?;

        #[cfg(test)]
        if self.fail_next_artifact_recheck.replace(false) {
            return Err(CanonicalTransactionErrorV1::not_published(
                "injected artifact closure recheck failure",
                self.store,
                session,
                expected,
                CandidateResidueV1::InstalledUnselected {
                    revision: revision.clone(),
                    durability: CandidateDurabilityV1::Confirmed,
                },
            ));
        }

        self.verify_artifact_closure(
            payload,
            CandidateResidueV1::InstalledUnselected {
                revision: revision.clone(),
                durability: CandidateDurabilityV1::Confirmed,
            },
        )?;

        #[cfg(test)]
        if self.fail_next_after_candidate.replace(false) {
            return Err(CanonicalTransactionErrorV1::not_published(
                "injected failure after immutable candidate installation",
                self.store,
                session,
                expected,
                CandidateResidueV1::InstalledUnselected {
                    revision: revision.clone(),
                    durability: CandidateDurabilityV1::Confirmed,
                },
            ));
        }

        let (receipt, durability_warning) =
            match self
                .store
                .compare_and_swap_head(&fence, expected.clone(), &candidate)
            {
                Ok(_) => (confirmed_receipt, None),
                Err(PhysicalHeadCasError::Publication {
                    message,
                    installed: true,
                }) => (unconfirmed_receipt, Some(message)),
                Err(error) => {
                    return Err(CanonicalTransactionErrorV1::not_published(
                        error.to_string(),
                        self.store,
                        session,
                        expected,
                        CandidateResidueV1::InstalledUnselected {
                            revision: revision.clone(),
                            durability: CandidateDurabilityV1::Confirmed,
                        },
                    ));
                }
            };
        Ok(CanonicalTransactionCommitV1 {
            outcome: SessionCommitOutcome::Published { receipt },
            durability_warning,
        })
    }

    fn receipt(
        &self,
        payload: &CanonicalSessionRevisionV1,
        durability: CommitDurability,
    ) -> Result<SessionCommitReceipt, CanonicalTransactionErrorV1> {
        SessionCommitReceipt::new(
            self.store.store_id().clone(),
            payload.session_id().clone(),
            payload.parent_revision().clone(),
            payload.revision_id().clone(),
            payload.selected_head().clone(),
            durability,
        )
        .map_err(|error| {
            CanonicalTransactionErrorV1::before_identity(format!(
                "cannot construct canonical commit receipt: {error}"
            ))
        })
    }

    fn verify_artifact_closure(
        &self,
        payload: &CanonicalSessionRevisionV1,
        candidate_residue: CandidateResidueV1,
    ) -> Result<(), CanonicalTransactionErrorV1> {
        for fact in payload.artifacts() {
            let verified = self
                .store
                .open_artifact(payload.session_id(), fact.artifact_id())
                .map_err(|message| {
                    CanonicalTransactionErrorV1::not_published(
                        format!(
                            "canonical artifact closure '{}' cannot be verified: {message}",
                            fact.artifact_id()
                        ),
                        self.store,
                        payload.session_id(),
                        payload.parent_revision(),
                        candidate_residue.clone(),
                    )
                })?;
            let manifest = verified.manifest();
            if !artifact_fact_matches_physical(fact, manifest) {
                return Err(CanonicalTransactionErrorV1::not_published(
                    format!(
                        "canonical artifact closure '{}' does not match the exact installed physical artifact",
                        fact.artifact_id()
                    ),
                    self.store,
                    payload.session_id(),
                    payload.parent_revision(),
                    candidate_residue,
                ));
            }
        }
        Ok(())
    }

    fn verify_artifact_closure_for_read(
        &self,
        payload: &CanonicalSessionRevisionV1,
    ) -> Result<(), CanonicalHeadReadErrorV1> {
        for fact in payload.artifacts() {
            let verified = self
                .store
                .open_artifact(payload.session_id(), fact.artifact_id())
                .map_err(|error| match error {
                    PhysicalArtifactReadErrorV1::UnsupportedPhysicalSchema {
                        observed,
                        supported,
                        message,
                    } => CanonicalHeadReadErrorV1::Unsupported {
                        observed_revision: Some(payload.revision_id().clone()),
                        dimension: SchemaDimension::Physical,
                        observed_schema: observed,
                        supported_schema: supported,
                        message,
                    },
                    PhysicalArtifactReadErrorV1::Missing(message) => {
                        CanonicalHeadReadErrorV1::Artifact {
                            observed_revision: payload.revision_id().clone(),
                            artifact_id: fact.artifact_id().to_string(),
                            reason: CorruptSessionReason::MissingReference,
                            message,
                        }
                    }
                    PhysicalArtifactReadErrorV1::Malformed(message) => {
                        CanonicalHeadReadErrorV1::Artifact {
                            observed_revision: payload.revision_id().clone(),
                            artifact_id: fact.artifact_id().to_string(),
                            reason: CorruptSessionReason::Malformed,
                            message,
                        }
                    }
                    PhysicalArtifactReadErrorV1::IdentityMismatch(message) => {
                        CanonicalHeadReadErrorV1::Artifact {
                            observed_revision: payload.revision_id().clone(),
                            artifact_id: fact.artifact_id().to_string(),
                            reason: CorruptSessionReason::IdentityMismatch,
                            message,
                        }
                    }
                    PhysicalArtifactReadErrorV1::DigestMismatch(message) => {
                        CanonicalHeadReadErrorV1::Artifact {
                            observed_revision: payload.revision_id().clone(),
                            artifact_id: fact.artifact_id().to_string(),
                            reason: CorruptSessionReason::DigestMismatch,
                            message,
                        }
                    }
                })?;
            let manifest = verified.manifest();
            if manifest.artifact_schema_revision() != fact.artifact_schema() {
                return Err(CanonicalHeadReadErrorV1::Unsupported {
                    observed_revision: Some(payload.revision_id().clone()),
                    dimension: SchemaDimension::Artifact,
                    observed_schema: manifest.artifact_schema_revision(),
                    supported_schema: fact.artifact_schema(),
                    message: format!(
                        "unsupported physical artifact schema {}; canonical revision requires {}",
                        manifest.artifact_schema_revision(),
                        fact.artifact_schema()
                    ),
                });
            }
            if !artifact_fact_matches_physical(fact, manifest) {
                return Err(CanonicalHeadReadErrorV1::Artifact {
                    observed_revision: payload.revision_id().clone(),
                    artifact_id: fact.artifact_id().to_string(),
                    reason: CorruptSessionReason::DigestMismatch,
                    message: "canonical fact does not match the exact installed physical artifact"
                        .to_string(),
                });
            }
        }
        Ok(())
    }

    fn install_error(
        &self,
        payload: &CanonicalSessionRevisionV1,
        canonical_bytes: &[u8],
        error: PhysicalCandidateInstallError,
    ) -> CanonicalTransactionErrorV1 {
        let residue = if error.installed() {
            CandidateResidueV1::InstalledUnselected {
                revision: payload.revision_id().clone(),
                durability: CandidateDurabilityV1::Unconfirmed,
            }
        } else if let Ok(existing) = self
            .store
            .open_revision_candidate(payload.session_id(), payload.revision_id())
        {
            if existing.canonical_bytes() == canonical_bytes {
                CandidateResidueV1::InstalledUnselected {
                    revision: payload.revision_id().clone(),
                    durability: CandidateDurabilityV1::Confirmed,
                }
            } else {
                CandidateResidueV1::PreexistingCollision {
                    revision: payload.revision_id().clone(),
                }
            }
        } else {
            CandidateResidueV1::None
        };
        CanonicalTransactionErrorV1::not_published(
            error.to_string(),
            self.store,
            payload.session_id(),
            payload.parent_revision(),
            residue,
        )
    }

    #[cfg(test)]
    fn inject_next_after_candidate_failure(&self) {
        self.fail_next_after_candidate.set(true);
    }

    #[cfg(test)]
    fn inject_next_artifact_recheck_failure(&self) {
        self.fail_next_artifact_recheck.set(true);
    }

    #[cfg(test)]
    fn inject_next_after_branch_artifacts_failure(&self) {
        self.fail_next_after_branch_artifacts.set(true);
    }
}

pub(super) fn lookup_canonical_mount_value_v1(
    store: &PhysicalFileStoreV1,
    session_id: &SessionId,
) -> (SessionLookupState, Option<CanonicalSessionRevisionV1>) {
    let lookup = CanonicalFileTransactionV1::new(store).lookup_canonical_head(session_id);
    (lookup.state, lookup.payload)
}

#[cfg(test)]
pub(super) fn publish_current_v1_mount_fixture_v1(
    store: &PhysicalFileStoreV1,
    source_bytes: &[u8],
    session_id: &SessionId,
    revision_id: RevisionId,
    committed_at: String,
) -> Result<(), String> {
    CanonicalFileTransactionV1::new(store)
        .commit_current_v1_legacy_baseline(
            source_bytes,
            session_id.as_str(),
            revision_id,
            committed_at,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(super) fn publish_mount_successor_fixture_v1(
    store: &PhysicalFileStoreV1,
    base: &CanonicalSessionRevisionV1,
    revision_id: RevisionId,
    committed_at: String,
) -> Result<(), String> {
    let terminal = kitsu_core::RunSpec::new(
        kitsu_core::RunId::new("run-mount-activation").map_err(|error| error.to_string())?,
    )
    .terminal(
        kitsu_core::RunOutcomeClass::Succeeded,
        kitsu_core::RunAssistantDisposition::Committed,
    );
    let input = CanonicalOperationInputV1::artifact_free_text(
        TurnId::new("turn-mount-activation").map_err(|error| error.to_string())?,
        terminal,
        "activation race".to_string(),
        Some("new selected value".to_string()),
    );
    CanonicalFileTransactionV1::new(store)
        .commit_artifact_free_operation(base, revision_id, committed_at, input)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kitsu_core::session::{
        CommitDurability, ExpectedRevision, RevisionId, SessionCommitReceipt, SessionId, StoreId,
        TurnId,
    };

    fn receipt(commit: &CanonicalTransactionCommitV1) -> &SessionCommitReceipt {
        match commit.outcome() {
            SessionCommitOutcome::Published { receipt } => receipt,
            SessionCommitOutcome::NotPublished { .. } => panic!("expected published outcome"),
        }
    }

    fn payload(
        store: &str,
        session: &str,
        revision: &str,
        parent: Option<&str>,
        turns: usize,
    ) -> crate::session::canonical::CanonicalSessionRevisionV1 {
        let parent = parent.map_or_else(
            || serde_json::json!({"kind":"missing"}),
            |revision| serde_json::json!({"kind":"exact","revision":revision}),
        );
        let operations = (1..=turns)
            .map(|turn| {
                let before = if turn == 1 {
                    serde_json::json!({"kind":"genesis"})
                } else {
                    serde_json::json!({"kind":"turn","turn":format!("turn-{}", turn - 1)})
                };
                serde_json::json!({
                    "operation": {
                        "turn_id": format!("turn-{turn}"),
                        "terminal": {
                            "run_id": format!("run-{turn}"),
                            "class": "succeeded",
                            "assistant": "committed"
                        },
                        "before_head": before,
                        "after_head": {"kind":"turn","turn":format!("turn-{turn}")}
                    },
                    "messages": [
                        {"role":"user","content":format!("question {turn}")},
                        {"role":"assistant","content":format!("answer {turn}"),"tool_calls":[]}
                    ],
                    "retained_effects": []
                })
            })
            .collect::<Vec<_>>();
        let value = serde_json::json!({
            "logical_schema": 2,
            "store_id": store,
            "session_id": session,
            "revision_id": revision,
            "parent_revision": parent,
            "created_at": "2026-08-20T10:00:00.000Z",
            "committed_at": "2026-08-20T10:01:00.000Z",
            "metadata": {
                "name": null,
                "launch_directory": "/work/tree",
                "requested_gateway": "openai",
                "requested_model": "gpt-test",
                "project_context_source": null
            },
            "lineage": {"kind":"root"},
            "operations": operations,
            "selected_head": {"kind":"turn","turn":format!("turn-{turns}")},
            "artifact_manifest": [],
            "compaction_binding": null,
            "recovery_log": []
        });
        crate::session::canonical::decode_canonical_payload(&serde_json::to_vec(&value).unwrap())
            .unwrap()
    }

    fn ids() -> (StoreId, SessionId, RevisionId, RevisionId) {
        (
            StoreId::new("store-1").unwrap(),
            SessionId::new("session-1").unwrap(),
            RevisionId::new("rev-a").unwrap(),
            RevisionId::new("rev-b").unwrap(),
        )
    }

    fn recovery_skips_match(
        actual: &[CanonicalRecoverySkipV1],
        expected: &[(&RevisionId, CorruptSessionReason)],
    ) -> bool {
        actual.len() == expected.len()
            && actual
                .iter()
                .zip(expected)
                .all(|(actual, (revision, reason))| {
                    actual.revision() == *revision && actual.reason() == *reason
                })
    }

    fn publish_invalid_head_successor(
        store: &PhysicalFileStoreV1,
        session_id: &SessionId,
        revision: &RevisionId,
        parent: ExpectedRevision,
        turns: usize,
    ) {
        let parent_id = match &parent {
            ExpectedRevision::Missing => None,
            ExpectedRevision::Exact(parent) => Some(parent.as_str()),
        };
        let mut corrupt = serde_json::to_value(payload(
            "store-1",
            session_id.as_str(),
            revision.as_str(),
            parent_id,
            turns,
        ))
        .unwrap();
        corrupt["selected_head"] =
            serde_json::json!({"kind": "turn", "turn": "turn-not-committed"});
        let candidate = store
            .install_revision_candidate(
                LOGICAL_SCHEMA_REVISION,
                session_id.clone(),
                revision.clone(),
                parent.clone(),
                &serde_json::to_vec(&corrupt).unwrap(),
            )
            .unwrap();
        let fence = store.acquire_writer_fence(session_id).unwrap();
        store
            .compare_and_swap_head(&fence, parent, &candidate)
            .unwrap();
    }

    fn current_v1(messages: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "id": "session-1",
            "created_at": "2026-08-20T10:00:00.000Z",
            "updated_at": "2026-08-20T10:01:00.000Z",
            "directory": "/work/tree",
            "gateway": "openai",
            "model": "gpt-test",
            "name": "kept_name",
            "project_context": "exact historical context",
            "messages": messages,
        }))
        .unwrap()
    }

    fn closed_current_v1() -> Vec<u8> {
        current_v1(serde_json::json!([
            {"role":"user","content":"inspect"},
            {"role":"assistant","content":"","tool_calls":[{
                "tool_name":"fs_read_file","args":"{\"path\":\"a.rs\"}","call_id":"call-1"
            }]},
            {"role":"tool","content":"contents","tool_call_id":"call-1"},
            {"role":"assistant","content":"done"}
        ]))
    }

    fn legacy_payload(revision: &str) -> crate::session::canonical::CanonicalSessionRevisionV1 {
        crate::session::canonical::prepare_current_v1_legacy_baseline(
            &closed_current_v1(),
            "session-1",
            StoreId::new("store-1").unwrap(),
            RevisionId::new(revision).unwrap(),
            "2026-08-20T11:00:00.000Z".to_string(),
        )
        .unwrap()
    }

    fn terminal(
        run: &str,
        class: kitsu_core::RunOutcomeClass,
        assistant: kitsu_core::RunAssistantDisposition,
    ) -> kitsu_core::TerminalOutcome {
        kitsu_core::RunSpec::new(kitsu_core::RunId::new(run).unwrap()).terminal(class, assistant)
    }

    fn text_operation_input(
        turn: &str,
        run: &str,
        class: kitsu_core::RunOutcomeClass,
        disposition: kitsu_core::RunAssistantDisposition,
        user: &str,
        assistant: Option<&str>,
    ) -> CanonicalOperationInputV1 {
        CanonicalOperationInputV1::artifact_free_text(
            kitsu_core::session::TurnId::new(turn).unwrap(),
            terminal(run, class, disposition),
            user.to_string(),
            assistant.map(str::to_string),
        )
    }

    fn artifact_operation_input(
        artifact_id: &str,
        sha256: &str,
        bytes: u64,
        media_type: Option<&str>,
    ) -> CanonicalOperationInputV1 {
        let artifact = ImmutableArtifactRefV1::new(
            1,
            artifact_id.to_string(),
            sha256.to_string(),
            bytes,
            media_type.map(str::to_string),
        )
        .unwrap();
        let tool_round = CanonicalToolRoundV1::new(
            "fs_read_file".to_string(),
            "{\"path\":\"artifact.txt\"}".to_string(),
            "call-artifact".to_string(),
            "immutable tool output".to_string(),
            "effect-artifact".to_string(),
            artifact,
        );
        CanonicalOperationInputV1::artifact_tool_round(
            kitsu_core::session::TurnId::new("turn-artifact").unwrap(),
            terminal(
                "run-artifact",
                kitsu_core::RunOutcomeClass::Succeeded,
                kitsu_core::RunAssistantDisposition::Committed,
            ),
            "inspect artifact".to_string(),
            "artifact inspected".to_string(),
            tool_round,
        )
    }

    fn committed_artifact_source(
        store: &PhysicalFileStoreV1,
        transaction: &CanonicalFileTransactionV1<'_>,
    ) -> CanonicalSessionRevisionV1 {
        let source_session = SessionId::new("session-1").unwrap();
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let artifact_bytes = b"branch artifact bytes";
        store
            .install_artifact(
                1,
                source_session.clone(),
                "artifact-branch".to_string(),
                Some("text/plain".to_string()),
                artifact_bytes,
            )
            .unwrap();
        store
            .install_artifact(
                1,
                source_session.clone(),
                "artifact-unreferenced".to_string(),
                None,
                b"must not cross the branch boundary",
            )
            .unwrap();
        transaction
            .commit_artifact_tool_operation(
                &base,
                RevisionId::new("rev-b").unwrap(),
                "2026-08-20T11:01:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-branch",
                    &crate::skills::content_sha256_hex(artifact_bytes),
                    artifact_bytes.len() as u64,
                    Some("text/plain"),
                ),
            )
            .unwrap();
        transaction
            .open_canonical_head(&source_session)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn branch_transaction_copies_exact_closure_and_publishes_create_only() {
        let root = tempfile::tempdir().unwrap();
        let store =
            PhysicalFileStoreV1::create_or_open(root.path(), StoreId::new("store-1").unwrap())
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let source = committed_artifact_source(&store, &transaction);
        let destination = SessionId::new("session-branch").unwrap();
        let destination_revision = RevisionId::new("rev-branch").unwrap();
        let branch_turn = TurnId::new("turn-artifact").unwrap();
        let expected = crate::session::canonical::assemble_branch_at_committed_turn(
            &source,
            store.store_id().clone(),
            destination.clone(),
            destination_revision.clone(),
            "2026-08-20T11:02:00.000Z".to_string(),
            branch_turn.clone(),
        )
        .unwrap();

        store.inject_next_head_sync_failure();
        let committed = transaction
            .commit_branch_at_turn(
                &source,
                destination.clone(),
                destination_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn,
            )
            .unwrap();

        assert_eq!(
            receipt(&committed).durability(),
            CommitDurability::PublishedUnconfirmed
        );
        assert!(committed.durability_warning().is_some());
        assert_eq!(
            transaction
                .open_canonical_head(&destination)
                .unwrap()
                .unwrap(),
            expected
        );
        assert_eq!(
            store
                .open_artifact(&destination, "artifact-branch")
                .unwrap()
                .content_bytes(),
            b"branch artifact bytes"
        );
        assert!(store
            .open_artifact(&destination, "artifact-unreferenced")
            .is_err());
        assert_eq!(
            store
                .read_head(source.session_id())
                .unwrap()
                .unwrap()
                .revision_id(),
            source.revision_id()
        );
    }

    #[test]
    fn branch_transaction_rejects_stale_and_collision_before_destination_head() {
        let root = tempfile::tempdir().unwrap();
        let store =
            PhysicalFileStoreV1::create_or_open(root.path(), StoreId::new("store-1").unwrap())
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let stale_source = committed_artifact_source(&store, &transaction);
        transaction
            .commit_artifact_tool_operation(
                &stale_source,
                RevisionId::new("rev-c").unwrap(),
                "2026-08-20T11:02:00.000Z".to_string(),
                text_operation_input(
                    "turn-after",
                    "run-after",
                    kitsu_core::RunOutcomeClass::Succeeded,
                    kitsu_core::RunAssistantDisposition::Committed,
                    "later",
                    Some("later answer"),
                ),
            )
            .unwrap();
        let destination = SessionId::new("session-stale-branch").unwrap();

        let stale = transaction
            .commit_branch_at_turn(
                &stale_source,
                destination.clone(),
                RevisionId::new("rev-stale-branch").unwrap(),
                "2026-08-20T11:03:00.000Z".to_string(),
                TurnId::new("turn-artifact").unwrap(),
            )
            .unwrap_err();
        assert!(stale.to_string().contains("source changed"));
        assert!(stale.artifact_residue().is_empty());
        assert!(store.read_head(&destination).unwrap().is_none());
        assert!(store
            .open_artifact(&destination, "artifact-branch")
            .is_err());

        let current_source = transaction
            .open_canonical_head(stale_source.session_id())
            .unwrap()
            .unwrap();
        let collision_destination = SessionId::new("session-collision-branch").unwrap();
        store
            .install_artifact(
                1,
                collision_destination.clone(),
                "artifact-branch".to_string(),
                Some("text/plain".to_string()),
                b"preexisting",
            )
            .unwrap();

        let collision = transaction
            .commit_branch_at_turn(
                &current_source,
                collision_destination.clone(),
                RevisionId::new("rev-collision-branch").unwrap(),
                "2026-08-20T11:04:00.000Z".to_string(),
                TurnId::new("turn-artifact").unwrap(),
            )
            .unwrap_err();
        assert!(collision.to_string().contains("already exists"));
        assert!(collision.artifact_residue().is_empty());
        assert!(store.read_head(&collision_destination).unwrap().is_none());
        assert_eq!(
            store
                .open_artifact(&collision_destination, "artifact-branch")
                .unwrap()
                .content_bytes(),
            b"preexisting"
        );

        let candidate_destination = SessionId::new("session-candidate-collision").unwrap();
        let candidate_revision = RevisionId::new("rev-candidate-collision").unwrap();
        store
            .install_revision_candidate(
                LOGICAL_SCHEMA_REVISION,
                candidate_destination.clone(),
                candidate_revision.clone(),
                ExpectedRevision::Missing,
                b"preexisting candidate",
            )
            .unwrap();
        let candidate_collision = transaction
            .commit_branch_at_turn(
                &current_source,
                candidate_destination.clone(),
                candidate_revision.clone(),
                "2026-08-20T11:04:00.000Z".to_string(),
                TurnId::new("turn-artifact").unwrap(),
            )
            .unwrap_err();
        assert!(candidate_collision
            .to_string()
            .contains("destination revision already exists"));
        assert!(candidate_collision.artifact_residue().is_empty());
        assert!(store.read_head(&candidate_destination).unwrap().is_none());
        assert!(store
            .open_artifact(&candidate_destination, "artifact-branch")
            .is_err());
        assert_eq!(
            store
                .open_revision_candidate(&candidate_destination, &candidate_revision)
                .unwrap()
                .canonical_bytes(),
            b"preexisting candidate"
        );
    }

    #[test]
    fn branch_transaction_cut_receipts_never_expose_an_incomplete_head() {
        let root = tempfile::tempdir().unwrap();
        let store =
            PhysicalFileStoreV1::create_or_open(root.path(), StoreId::new("store-1").unwrap())
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let source = committed_artifact_source(&store, &transaction);

        let copy_sync_cut_session = SessionId::new("session-copy-sync-cut").unwrap();
        store.inject_next_target_sync_failure();
        let copy_sync_cut = transaction
            .commit_branch_at_turn(
                &source,
                copy_sync_cut_session.clone(),
                RevisionId::new("rev-copy-sync-cut").unwrap(),
                "2026-08-20T11:02:00.000Z".to_string(),
                TurnId::new("turn-artifact").unwrap(),
            )
            .unwrap_err();
        assert_eq!(
            copy_sync_cut.artifact_residue(),
            [ArtifactResidueV1::Installed {
                artifact_id: "artifact-branch".to_string(),
                durability: CandidateDurabilityV1::Unconfirmed,
            }]
        );
        assert_eq!(copy_sync_cut.candidate_residue(), CandidateResidueV1::None);
        assert!(store.read_head(&copy_sync_cut_session).unwrap().is_none());
        assert!(store
            .open_artifact(&copy_sync_cut_session, "artifact-branch")
            .is_ok());

        let artifact_cut_session = SessionId::new("session-artifact-cut").unwrap();
        transaction.inject_next_after_branch_artifacts_failure();
        let artifact_cut = transaction
            .commit_branch_at_turn(
                &source,
                artifact_cut_session.clone(),
                RevisionId::new("rev-artifact-cut").unwrap(),
                "2026-08-20T11:02:00.000Z".to_string(),
                TurnId::new("turn-artifact").unwrap(),
            )
            .unwrap_err();
        assert_eq!(
            artifact_cut.artifact_residue(),
            [ArtifactResidueV1::Installed {
                artifact_id: "artifact-branch".to_string(),
                durability: CandidateDurabilityV1::Confirmed,
            }]
        );
        assert_eq!(artifact_cut.candidate_residue(), CandidateResidueV1::None);
        assert!(store.read_head(&artifact_cut_session).unwrap().is_none());

        let candidate_cut_session = SessionId::new("session-candidate-cut").unwrap();
        transaction.inject_next_after_candidate_failure();
        let candidate_cut = transaction
            .commit_branch_at_turn(
                &source,
                candidate_cut_session.clone(),
                RevisionId::new("rev-candidate-cut").unwrap(),
                "2026-08-20T11:02:00.000Z".to_string(),
                TurnId::new("turn-artifact").unwrap(),
            )
            .unwrap_err();
        assert_eq!(
            candidate_cut.artifact_residue(),
            [ArtifactResidueV1::Installed {
                artifact_id: "artifact-branch".to_string(),
                durability: CandidateDurabilityV1::Confirmed,
            }]
        );
        assert_eq!(
            candidate_cut.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: RevisionId::new("rev-candidate-cut").unwrap(),
                durability: CandidateDurabilityV1::Confirmed,
            }
        );
        assert!(store.read_head(&candidate_cut_session).unwrap().is_none());

        let recheck_cut_session = SessionId::new("session-recheck-cut").unwrap();
        transaction.inject_next_artifact_recheck_failure();
        let recheck_cut = transaction
            .commit_branch_at_turn(
                &source,
                recheck_cut_session.clone(),
                RevisionId::new("rev-recheck-cut").unwrap(),
                "2026-08-20T11:02:00.000Z".to_string(),
                TurnId::new("turn-artifact").unwrap(),
            )
            .unwrap_err();
        assert_eq!(
            recheck_cut.artifact_residue(),
            [ArtifactResidueV1::Installed {
                artifact_id: "artifact-branch".to_string(),
                durability: CandidateDurabilityV1::Confirmed,
            }]
        );
        assert_eq!(
            recheck_cut.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: RevisionId::new("rev-recheck-cut").unwrap(),
                durability: CandidateDurabilityV1::Confirmed,
            }
        );
        assert!(store.read_head(&recheck_cut_session).unwrap().is_none());
    }

    #[test]
    fn branch_transaction_retries_only_exact_verified_residue() {
        let root = tempfile::tempdir().unwrap();
        let store =
            PhysicalFileStoreV1::create_or_open(root.path(), StoreId::new("store-1").unwrap())
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let source = committed_artifact_source(&store, &transaction);
        let branch_turn = TurnId::new("turn-artifact").unwrap();

        let artifact_session = SessionId::new("session-retry-artifact").unwrap();
        let artifact_revision = RevisionId::new("rev-retry-artifact").unwrap();
        transaction.inject_next_after_branch_artifacts_failure();
        transaction
            .commit_branch_at_turn(
                &source,
                artifact_session.clone(),
                artifact_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn.clone(),
            )
            .unwrap_err();
        let artifact_retry = transaction
            .commit_branch_at_turn(
                &source,
                artifact_session.clone(),
                artifact_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn.clone(),
            )
            .unwrap();
        assert_eq!(receipt(&artifact_retry).new_revision(), &artifact_revision);
        assert_eq!(
            store
                .read_head(&artifact_session)
                .unwrap()
                .unwrap()
                .revision_id(),
            &artifact_revision
        );

        let candidate_session = SessionId::new("session-retry-candidate").unwrap();
        let candidate_revision = RevisionId::new("rev-retry-candidate").unwrap();
        transaction.inject_next_after_candidate_failure();
        transaction
            .commit_branch_at_turn(
                &source,
                candidate_session.clone(),
                candidate_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn.clone(),
            )
            .unwrap_err();
        transaction
            .commit_branch_at_turn(
                &source,
                candidate_session.clone(),
                candidate_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn.clone(),
            )
            .unwrap();
        assert_eq!(
            store
                .read_head(&candidate_session)
                .unwrap()
                .unwrap()
                .revision_id(),
            &candidate_revision
        );

        let candidate_sync_session = SessionId::new("session-retry-candidate-sync").unwrap();
        let candidate_sync_revision = RevisionId::new("rev-retry-candidate-sync").unwrap();
        transaction.inject_next_after_branch_artifacts_failure();
        transaction
            .commit_branch_at_turn(
                &source,
                candidate_sync_session.clone(),
                candidate_sync_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn.clone(),
            )
            .unwrap_err();
        store.inject_next_target_sync_failure();
        let candidate_sync = transaction
            .commit_branch_at_turn(
                &source,
                candidate_sync_session.clone(),
                candidate_sync_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn.clone(),
            )
            .unwrap_err();
        assert_eq!(
            candidate_sync.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: candidate_sync_revision.clone(),
                durability: CandidateDurabilityV1::Unconfirmed,
            }
        );
        assert!(store.read_head(&candidate_sync_session).unwrap().is_none());
        transaction
            .commit_branch_at_turn(
                &source,
                candidate_sync_session.clone(),
                candidate_sync_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn.clone(),
            )
            .unwrap();
        assert_eq!(
            store
                .read_head(&candidate_sync_session)
                .unwrap()
                .unwrap()
                .revision_id(),
            &candidate_sync_revision
        );

        let unconfirmed_session = SessionId::new("session-retry-unconfirmed").unwrap();
        let unconfirmed_revision = RevisionId::new("rev-retry-unconfirmed").unwrap();
        store.inject_next_target_sync_failure();
        let unconfirmed = transaction
            .commit_branch_at_turn(
                &source,
                unconfirmed_session.clone(),
                unconfirmed_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn.clone(),
            )
            .unwrap_err();
        assert!(matches!(
            unconfirmed.artifact_residue(),
            [ArtifactResidueV1::Installed {
                durability: CandidateDurabilityV1::Unconfirmed,
                ..
            }]
        ));
        transaction
            .commit_branch_at_turn(
                &source,
                unconfirmed_session.clone(),
                unconfirmed_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn.clone(),
            )
            .unwrap();
        assert_eq!(
            store
                .read_head(&unconfirmed_session)
                .unwrap()
                .unwrap()
                .revision_id(),
            &unconfirmed_revision
        );

        let tampered_session = SessionId::new("session-retry-tampered").unwrap();
        let tampered_revision = RevisionId::new("rev-retry-tampered").unwrap();
        transaction.inject_next_after_candidate_failure();
        transaction
            .commit_branch_at_turn(
                &source,
                tampered_session.clone(),
                tampered_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn.clone(),
            )
            .unwrap_err();
        std::fs::write(
            crate::session::physical::PhysicalLayoutV1::artifact_content(
                root.path(),
                &tampered_session,
                "artifact-branch",
            ),
            b"tampered residue",
        )
        .unwrap();
        let tampered = transaction
            .commit_branch_at_turn(
                &source,
                tampered_session.clone(),
                tampered_revision,
                "2026-08-20T11:02:00.000Z".to_string(),
                branch_turn,
            )
            .unwrap_err();
        assert!(tampered.to_string().contains("destination artifact"));
        assert!(store.read_head(&tampered_session).unwrap().is_none());
    }

    #[test]
    fn canonical_head_read_accepts_artifact_free_revision() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, _) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction.commit(&legacy_payload("rev-a")).unwrap();

        let opened = transaction
            .open_canonical_head(&session_id)
            .unwrap()
            .unwrap();

        assert_eq!(opened.revision_id(), &rev_a);
        assert_eq!(opened.artifact_count(), 0);
    }

    #[test]
    fn canonical_head_read_returns_none_only_when_head_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, _) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);

        assert!(transaction
            .open_canonical_head(&session_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn canonical_head_read_rejects_logical_physical_identity_disagreement() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let mismatched = payload("other-store", "session-1", "rev-b", None, 1);
        let mismatched_bytes = encode_canonical_payload(&mismatched).unwrap();
        let candidate = store
            .install_revision_candidate(
                LOGICAL_SCHEMA_REVISION,
                session_id.clone(),
                rev_b.clone(),
                ExpectedRevision::Missing,
                &mismatched_bytes,
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session_id).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Missing, &candidate)
            .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);

        let error = transaction.open_canonical_head(&session_id).unwrap_err();

        assert!(matches!(error, CanonicalHeadReadErrorV1::Binding { .. }));
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
    }

    #[test]
    fn canonical_head_read_reports_invalid_logical_payload_without_fallback() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let candidate = store
            .install_revision_candidate(
                LOGICAL_SCHEMA_REVISION,
                session_id.clone(),
                rev_b.clone(),
                ExpectedRevision::Missing,
                b"not a canonical session payload",
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session_id).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Missing, &candidate)
            .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);

        let error = transaction.open_canonical_head(&session_id).unwrap_err();

        assert!(matches!(error, CanonicalHeadReadErrorV1::Logical { .. }));
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
    }

    #[test]
    fn canonical_lookup_distinguishes_invalid_selected_head_from_broken_causality() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let mut invalid_head =
            serde_json::to_value(payload("store-1", "session-1", "rev-b", None, 1)).unwrap();
        invalid_head["selected_head"] =
            serde_json::json!({"kind": "turn", "turn": "turn-not-committed"});
        let candidate = store
            .install_revision_candidate(
                LOGICAL_SCHEMA_REVISION,
                session_id.clone(),
                rev_b.clone(),
                ExpectedRevision::Missing,
                &serde_json::to_vec(&invalid_head).unwrap(),
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session_id).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Missing, &candidate)
            .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);

        let lookup = transaction.lookup_canonical_head(&session_id);

        assert_eq!(
            lookup.state(),
            &SessionLookupState::Corrupt {
                session: session_id,
                observed_revision: Some(rev_b),
                reason: CorruptSessionReason::InvalidHead
            }
        );
        assert!(lookup.payload().is_none());
    }

    #[test]
    fn canonical_lookup_classifies_physical_head_and_revision_faults_without_messages() {
        for (fault, expected_state) in [
            (
                "future-head-schema",
                SessionLookupState::Unsupported {
                    dimension: SchemaDimension::Physical,
                    observed_revision: 3,
                    supported_revision: 2,
                },
            ),
            (
                "zero-head-schema",
                SessionLookupState::Corrupt {
                    session: SessionId::new("session-1").unwrap(),
                    observed_revision: Some(RevisionId::new("rev-a").unwrap()),
                    reason: CorruptSessionReason::Malformed,
                },
            ),
            (
                "head-identity",
                SessionLookupState::Corrupt {
                    session: SessionId::new("session-1").unwrap(),
                    observed_revision: Some(RevisionId::new("rev-a").unwrap()),
                    reason: CorruptSessionReason::IdentityMismatch,
                },
            ),
            (
                "missing-revision",
                SessionLookupState::Corrupt {
                    session: SessionId::new("session-1").unwrap(),
                    observed_revision: Some(RevisionId::new("rev-a").unwrap()),
                    reason: CorruptSessionReason::MissingReference,
                },
            ),
            (
                "future-revision-schema",
                SessionLookupState::Unsupported {
                    dimension: SchemaDimension::Physical,
                    observed_revision: 3,
                    supported_revision: 2,
                },
            ),
            (
                "zero-revision-schema",
                SessionLookupState::Corrupt {
                    session: SessionId::new("session-1").unwrap(),
                    observed_revision: Some(RevisionId::new("rev-a").unwrap()),
                    reason: CorruptSessionReason::Malformed,
                },
            ),
            (
                "revision-identity",
                SessionLookupState::Corrupt {
                    session: SessionId::new("session-1").unwrap(),
                    observed_revision: Some(RevisionId::new("rev-a").unwrap()),
                    reason: CorruptSessionReason::IdentityMismatch,
                },
            ),
            (
                "head-manifest-digest",
                SessionLookupState::Corrupt {
                    session: SessionId::new("session-1").unwrap(),
                    observed_revision: Some(RevisionId::new("rev-a").unwrap()),
                    reason: CorruptSessionReason::DigestMismatch,
                },
            ),
            (
                "canonical-digest",
                SessionLookupState::Corrupt {
                    session: SessionId::new("session-1").unwrap(),
                    observed_revision: Some(RevisionId::new("rev-a").unwrap()),
                    reason: CorruptSessionReason::DigestMismatch,
                },
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (store_id, session_id, rev_a, _) = ids();
            let store = crate::session::physical::PhysicalFileStoreV1::create_or_open(
                root.path(),
                store_id,
            )
            .unwrap();
            let transaction = CanonicalFileTransactionV1::new(&store);
            transaction.commit(&legacy_payload("rev-a")).unwrap();

            match fault {
                "future-head-schema" | "zero-head-schema" => {
                    let path = crate::session::physical::PhysicalLayoutV1::head_pointer(
                        root.path(),
                        &session_id,
                    );
                    let mut value: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    value["physical_schema_revision"] =
                        serde_json::json!(if fault == "future-head-schema" { 3 } else { 0 });
                    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                }
                "head-identity" => {
                    let path = crate::session::physical::PhysicalLayoutV1::head_pointer(
                        root.path(),
                        &session_id,
                    );
                    let mut value: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    value["session_id"] = serde_json::json!("other-session");
                    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                }
                "missing-revision" => {
                    std::fs::remove_dir_all(
                        crate::session::physical::PhysicalLayoutV1::revision_directory(
                            root.path(),
                            &session_id,
                            &rev_a,
                        ),
                    )
                    .unwrap();
                }
                "future-revision-schema" | "zero-revision-schema" => {
                    let path = crate::session::physical::PhysicalLayoutV1::revision_manifest(
                        root.path(),
                        &session_id,
                        &rev_a,
                    );
                    let mut value: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    value["physical_schema_revision"] =
                        serde_json::json!(if fault == "future-revision-schema" {
                            3
                        } else {
                            0
                        });
                    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                }
                "revision-identity" => {
                    let path = crate::session::physical::PhysicalLayoutV1::revision_manifest(
                        root.path(),
                        &session_id,
                        &rev_a,
                    );
                    let mut value: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    value["session_id"] = serde_json::json!("other-session");
                    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                }
                "head-manifest-digest" => {
                    let path = crate::session::physical::PhysicalLayoutV1::head_pointer(
                        root.path(),
                        &session_id,
                    );
                    let mut value: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    value["revision_manifest_sha256"] = serde_json::json!("f".repeat(64));
                    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                }
                "canonical-digest" => {
                    let path = crate::session::physical::PhysicalLayoutV1::canonical_payload(
                        root.path(),
                        &session_id,
                        &rev_a,
                    );
                    let mut bytes = std::fs::read(&path).unwrap();
                    let index = bytes.iter().position(|byte| *byte == b'1').unwrap();
                    bytes[index] = b'2';
                    std::fs::write(path, bytes).unwrap();
                }
                _ => unreachable!(),
            }

            let lookup = transaction.lookup_canonical_head(&session_id);

            assert_eq!(lookup.state(), &expected_state, "fault: {fault}");
            assert!(lookup.payload().is_none(), "fault: {fault}");
            assert!(
                matches!(
                    transaction.plan_last_good_recovery(&session_id),
                    CanonicalRecoveryPlanV1::Unavailable {
                        selected: Some(ref selected),
                        ref selected_state,
                        ..
                    } if selected == &rev_a && selected_state == &expected_state
                ),
                "recovery fault: {fault}"
            );
        }
    }

    #[test]
    fn canonical_lineage_follows_only_selected_parents_and_ignores_orphans() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        transaction
            .commit(&payload("store-1", "session-1", "rev-b", Some("rev-a"), 2))
            .unwrap();
        let rev_c = RevisionId::new("rev-c").unwrap();
        let orphan = payload("store-1", "session-1", "rev-c", Some("rev-b"), 3);
        store
            .install_revision_candidate(
                LOGICAL_SCHEMA_REVISION,
                session_id.clone(),
                rev_c.clone(),
                ExpectedRevision::Exact(rev_b.clone()),
                &encode_canonical_payload(&orphan).unwrap(),
            )
            .unwrap();

        let lookup = transaction.lookup_canonical_lineage(&session_id);

        assert_eq!(
            lookup.state(),
            &SessionLookupState::Present {
                revision: rev_b.clone(),
                head: kitsu_core::session::LogicalHead::Turn(
                    kitsu_core::session::TurnId::new("turn-2").unwrap()
                )
            }
        );
        let revisions = lookup.lineage().unwrap().revisions();
        assert_eq!(
            revisions
                .iter()
                .map(|payload| payload.revision_id().as_str())
                .collect::<Vec<_>>(),
            vec!["rev-b", "rev-a"]
        );
        assert_eq!(
            revisions[0].parent_manifest_sha256.as_deref(),
            Some(revisions[1].manifest_sha256.as_str())
        );
        assert!(revisions[1].parent_manifest_sha256.is_none());
        assert!(store.open_revision_candidate(&session_id, &rev_c).is_ok());
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
        assert_eq!(revisions.last().unwrap().revision_id(), &rev_a);
    }

    #[test]
    fn canonical_lineage_bound_fails_closed_without_reselecting_head() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        transaction
            .commit(&payload("store-1", "session-1", "rev-b", Some("rev-a"), 2))
            .unwrap();

        let lookup = transaction.lookup_canonical_lineage_bounded(&session_id, 1);

        assert_eq!(
            lookup.state(),
            &SessionLookupState::Corrupt {
                session: session_id.clone(),
                observed_revision: Some(rev_a),
                reason: CorruptSessionReason::BrokenCausality
            }
        );
        assert!(lookup.lineage().is_none());
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
    }

    #[test]
    fn canonical_lineage_reports_a_missing_ancestor_without_fallback() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        transaction
            .commit(&payload("store-1", "session-1", "rev-b", Some("rev-a"), 2))
            .unwrap();
        std::fs::remove_dir_all(
            crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &rev_a,
            ),
        )
        .unwrap();

        let lookup = transaction.lookup_canonical_lineage(&session_id);

        assert_eq!(
            lookup.state(),
            &SessionLookupState::Corrupt {
                session: session_id.clone(),
                observed_revision: Some(rev_a),
                reason: CorruptSessionReason::MissingReference
            }
        );
        assert!(lookup.lineage().is_none());
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
    }

    #[test]
    fn canonical_lineage_rejects_a_self_consistent_parent_rewrite() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store = crate::session::physical::PhysicalFileStoreV1::create_or_open(
            root.path(),
            store_id.clone(),
        )
        .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        transaction
            .commit(&payload("store-1", "session-1", "rev-b", Some("rev-a"), 2))
            .unwrap();
        let replacement =
            encode_canonical_payload(&payload("store-1", "session-1", "rev-a", None, 2)).unwrap();
        let replacement_digest = crate::session::physical::Sha256Digest::new(
            crate::skills::content_sha256_hex(&replacement),
        )
        .unwrap();
        let replacement_manifest = crate::session::physical::RevisionManifestV1::new(
            LOGICAL_SCHEMA_REVISION,
            store_id,
            session_id.clone(),
            rev_a.clone(),
            ExpectedRevision::Missing,
            None,
            replacement.len() as u64,
            replacement_digest,
        )
        .unwrap();
        std::fs::write(
            crate::session::physical::PhysicalLayoutV1::canonical_payload(
                root.path(),
                &session_id,
                &rev_a,
            ),
            replacement,
        )
        .unwrap();
        std::fs::write(
            crate::session::physical::PhysicalLayoutV1::revision_manifest(
                root.path(),
                &session_id,
                &rev_a,
            ),
            crate::session::physical::encode_revision_manifest(&replacement_manifest).unwrap(),
        )
        .unwrap();

        let lookup = transaction.lookup_canonical_lineage(&session_id);

        assert_eq!(
            lookup.state(),
            &SessionLookupState::Corrupt {
                session: session_id.clone(),
                observed_revision: Some(rev_a),
                reason: CorruptSessionReason::DigestMismatch
            }
        );
        assert!(lookup.lineage().is_none());
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
    }

    #[test]
    fn canonical_lineage_cycle_cannot_be_seeded_without_an_exact_parent() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();

        let error = store
            .install_revision_candidate(
                LOGICAL_SCHEMA_REVISION,
                session_id.clone(),
                rev_a.clone(),
                ExpectedRevision::Exact(rev_b),
                &encode_canonical_payload(&payload(
                    "store-1",
                    "session-1",
                    "rev-a",
                    Some("rev-b"),
                    1,
                ))
                .unwrap(),
            )
            .unwrap_err();

        assert!(!error.installed());
        assert!(error
            .to_string()
            .contains("cannot bind exact parent revision"));
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &rev_a,
            )
            .exists()
        );
        assert!(store.read_head(&session_id).unwrap().is_none());
    }

    #[test]
    fn recovery_plan_selects_verified_parent_without_changing_corrupt_head() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        let mut corrupt =
            serde_json::to_value(payload("store-1", "session-1", "rev-b", Some("rev-a"), 2))
                .unwrap();
        corrupt["selected_head"] =
            serde_json::json!({"kind": "turn", "turn": "turn-not-committed"});
        let candidate = store
            .install_revision_candidate(
                LOGICAL_SCHEMA_REVISION,
                session_id.clone(),
                rev_b.clone(),
                ExpectedRevision::Exact(rev_a.clone()),
                &serde_json::to_vec(&corrupt).unwrap(),
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session_id).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Exact(rev_a.clone()), &candidate)
            .unwrap();

        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &SessionLookupState::Corrupt {
                session: session_id.clone(),
                observed_revision: Some(rev_b.clone()),
                reason: CorruptSessionReason::InvalidHead
            }
        );
        let plan = transaction.plan_last_good_recovery(&session_id);

        assert!(matches!(
            plan,
            CanonicalRecoveryPlanV1::Available {
                ref selected,
                ref selected_state,
                ref recovery,
                ref recovery_head,
                ref skipped,
                ..
            } if selected == &rev_b
                && selected_state == &SessionLookupState::Corrupt {
                    session: session_id.clone(),
                    observed_revision: Some(rev_b.clone()),
                    reason: CorruptSessionReason::InvalidHead,
                }
                && recovery == &rev_a
                && recovery_head == &kitsu_core::session::LogicalHead::Turn(
                    kitsu_core::session::TurnId::new("turn-1").unwrap()
                )
                && recovery_skips_match(
                    skipped,
                    &[(&rev_b, CorruptSessionReason::InvalidHead)]
                )
        ));
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &SessionLookupState::Corrupt {
                session: session_id,
                observed_revision: Some(rev_b),
                reason: CorruptSessionReason::InvalidHead,
            }
        );
    }

    #[test]
    fn recovery_plan_is_unavailable_without_a_verified_parent() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        assert!(matches!(
            transaction.plan_last_good_recovery(&session_id),
            CanonicalRecoveryPlanV1::Missing
        ));
        let mut corrupt =
            serde_json::to_value(payload("store-1", "session-1", "rev-b", None, 1)).unwrap();
        corrupt["selected_head"] =
            serde_json::json!({"kind": "turn", "turn": "turn-not-committed"});
        let candidate = store
            .install_revision_candidate(
                LOGICAL_SCHEMA_REVISION,
                session_id.clone(),
                rev_b.clone(),
                ExpectedRevision::Missing,
                &serde_json::to_vec(&corrupt).unwrap(),
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session_id).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Missing, &candidate)
            .unwrap();

        let plan = transaction.plan_last_good_recovery(&session_id);

        assert!(matches!(
            plan,
            CanonicalRecoveryPlanV1::Unavailable {
                selected: Some(ref selected),
                ref selected_state,
                ref skipped,
                blocker: CanonicalRecoveryBlockerV1::ExhaustedAtRoot { ref revision },
            } if selected == &rev_b
                && selected_state == &SessionLookupState::Corrupt {
                    session: session_id.clone(),
                    observed_revision: Some(rev_b.clone()),
                    reason: CorruptSessionReason::InvalidHead,
                }
                && revision == &rev_b
                && recovery_skips_match(
                    skipped,
                    &[(&rev_b, CorruptSessionReason::InvalidHead)]
                )
        ));
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
    }

    #[test]
    fn recovery_plan_does_not_downgrade_valid_or_future_selected_state() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        assert!(matches!(
            transaction.plan_last_good_recovery(&session_id),
            CanonicalRecoveryPlanV1::NotNeeded {
                ref selected,
                ref head,
            } if selected == &rev_a
                && head == &kitsu_core::session::LogicalHead::Turn(
                    kitsu_core::session::TurnId::new("turn-1").unwrap()
                )
        ));

        let mut future =
            serde_json::to_value(payload("store-1", "session-1", "rev-b", Some("rev-a"), 2))
                .unwrap();
        future["logical_schema"] = serde_json::json!(3);
        let candidate = store
            .install_revision_candidate(
                3,
                session_id.clone(),
                rev_b.clone(),
                ExpectedRevision::Exact(rev_a.clone()),
                &serde_json::to_vec(&future).unwrap(),
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session_id).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Exact(rev_a), &candidate)
            .unwrap();

        let plan = transaction.plan_last_good_recovery(&session_id);

        assert!(matches!(
            plan,
            CanonicalRecoveryPlanV1::Unavailable {
                selected: Some(ref selected),
                ref selected_state,
                ref skipped,
                blocker: CanonicalRecoveryBlockerV1::SelectedState,
            } if selected == &rev_b
                && selected_state == &SessionLookupState::Unsupported {
                    dimension: SchemaDimension::Logical,
                    observed_revision: 3,
                    supported_revision: 2,
                }
                && skipped.is_empty()
        ));
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
    }

    #[test]
    fn recovery_plan_crosses_corrupt_parent_to_verified_grandparent() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let rev_c = RevisionId::new("rev-c").unwrap();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();

        for (revision, parent, turn) in [
            (rev_b.clone(), rev_a.clone(), 2),
            (rev_c.clone(), rev_b.clone(), 3),
        ] {
            let mut corrupt = serde_json::to_value(payload(
                "store-1",
                "session-1",
                revision.as_str(),
                Some(parent.as_str()),
                turn,
            ))
            .unwrap();
            corrupt["selected_head"] =
                serde_json::json!({"kind": "turn", "turn": "turn-not-committed"});
            let candidate = store
                .install_revision_candidate(
                    LOGICAL_SCHEMA_REVISION,
                    session_id.clone(),
                    revision.clone(),
                    ExpectedRevision::Exact(parent.clone()),
                    &serde_json::to_vec(&corrupt).unwrap(),
                )
                .unwrap();
            let fence = store.acquire_writer_fence(&session_id).unwrap();
            store
                .compare_and_swap_head(&fence, ExpectedRevision::Exact(parent), &candidate)
                .unwrap();
        }

        assert!(matches!(
            transaction.plan_last_good_recovery(&session_id),
            CanonicalRecoveryPlanV1::Available {
                ref selected,
                ref selected_state,
                ref recovery,
                ref recovery_head,
                ref skipped,
                ..
            } if selected == &rev_c
                && selected_state == &SessionLookupState::Corrupt {
                    session: session_id.clone(),
                    observed_revision: Some(rev_c.clone()),
                    reason: CorruptSessionReason::InvalidHead,
                }
                && recovery == &rev_a
                && recovery_head == &kitsu_core::session::LogicalHead::Turn(
                    kitsu_core::session::TurnId::new("turn-1").unwrap()
                )
                && recovery_skips_match(
                    skipped,
                    &[
                        (&rev_c, CorruptSessionReason::InvalidHead),
                        (&rev_b, CorruptSessionReason::InvalidHead),
                    ]
                )
        ));
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_c
        );
    }

    #[test]
    fn recovery_plan_uses_head_bound_manifest_when_selected_payload_is_unreadable() {
        for (fault, expected_reason) in [
            ("missing", CorruptSessionReason::MissingReference),
            ("digest", CorruptSessionReason::DigestMismatch),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (store_id, session_id, rev_a, rev_b) = ids();
            let store = crate::session::physical::PhysicalFileStoreV1::create_or_open(
                root.path(),
                store_id,
            )
            .unwrap();
            let transaction = CanonicalFileTransactionV1::new(&store);
            transaction
                .commit(&payload("store-1", "session-1", "rev-a", None, 1))
                .unwrap();
            transaction
                .commit(&payload("store-1", "session-1", "rev-b", Some("rev-a"), 2))
                .unwrap();
            let canonical_path = crate::session::physical::PhysicalLayoutV1::canonical_payload(
                root.path(),
                &session_id,
                &rev_b,
            );
            if fault == "missing" {
                std::fs::remove_file(canonical_path).unwrap();
            } else {
                std::fs::write(canonical_path, b"tampered canonical bytes").unwrap();
            }

            let selected_state = SessionLookupState::Corrupt {
                session: session_id.clone(),
                observed_revision: Some(rev_b.clone()),
                reason: expected_reason,
            };
            assert_eq!(
                transaction.lookup_canonical_head(&session_id).state(),
                &selected_state,
                "fault: {fault}"
            );
            let envelope = store.read_head_envelope(&session_id).unwrap().unwrap();
            assert_eq!(envelope.revision_id(), &rev_b, "fault: {fault}");
            assert_eq!(
                envelope.manifest().parent_revision(),
                &ExpectedRevision::Exact(rev_a.clone()),
                "fault: {fault}"
            );

            assert!(matches!(
                transaction.plan_last_good_recovery(&session_id),
                CanonicalRecoveryPlanV1::Available {
                    ref selected,
                    selected_state: ref observed_state,
                    ref recovery,
                    ref recovery_head,
                    ref skipped,
                    ..
                } if selected == &rev_b
                    && observed_state == &selected_state
                    && recovery == &rev_a
                    && recovery_head == &kitsu_core::session::LogicalHead::Turn(
                        kitsu_core::session::TurnId::new("turn-1").unwrap()
                    )
                    && recovery_skips_match(skipped, &[(&rev_b, expected_reason)])
            ));
            assert_eq!(
                store
                    .read_head_envelope(&session_id)
                    .unwrap()
                    .unwrap()
                    .revision_id(),
                &rev_b,
                "fault: {fault}"
            );
            assert_eq!(
                transaction.lookup_canonical_head(&session_id).state(),
                &selected_state,
                "fault: {fault}"
            );
        }
    }

    #[test]
    fn recovery_plan_crosses_unreadable_parent_only_through_bound_manifests() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let rev_c = RevisionId::new("rev-c").unwrap();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        for value in [
            payload("store-1", "session-1", "rev-a", None, 1),
            payload("store-1", "session-1", "rev-b", Some("rev-a"), 2),
            payload("store-1", "session-1", "rev-c", Some("rev-b"), 3),
        ] {
            transaction.commit(&value).unwrap();
        }
        for revision in [&rev_b, &rev_c] {
            std::fs::write(
                crate::session::physical::PhysicalLayoutV1::canonical_payload(
                    root.path(),
                    &session_id,
                    revision,
                ),
                b"tampered canonical bytes",
            )
            .unwrap();
        }
        let selected_state = SessionLookupState::Corrupt {
            session: session_id.clone(),
            observed_revision: Some(rev_c.clone()),
            reason: CorruptSessionReason::DigestMismatch,
        };

        assert!(matches!(
            transaction.plan_last_good_recovery(&session_id),
            CanonicalRecoveryPlanV1::Available {
                ref selected,
                selected_state: ref observed_state,
                ref recovery,
                ref recovery_head,
                ref skipped,
                ..
            } if selected == &rev_c
                && observed_state == &selected_state
                && recovery == &rev_a
                && recovery_head == &kitsu_core::session::LogicalHead::Turn(
                    kitsu_core::session::TurnId::new("turn-1").unwrap()
                )
                && recovery_skips_match(
                    skipped,
                    &[
                        (&rev_c, CorruptSessionReason::DigestMismatch),
                        (&rev_b, CorruptSessionReason::DigestMismatch),
                    ]
                )
        ));
        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &selected_state
        );
    }

    #[test]
    fn recovery_plan_refuses_unverified_selected_manifest_despite_valid_parent() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        transaction
            .commit(&payload("store-1", "session-1", "rev-b", Some("rev-a"), 2))
            .unwrap();
        let manifest_path = crate::session::physical::PhysicalLayoutV1::revision_manifest(
            root.path(),
            &session_id,
            &rev_b,
        );
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["canonical_sha256"] = serde_json::json!("f".repeat(64));
        std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let selected_state = SessionLookupState::Corrupt {
            session: session_id.clone(),
            observed_revision: Some(rev_b.clone()),
            reason: CorruptSessionReason::DigestMismatch,
        };

        assert!(matches!(
            transaction.plan_last_good_recovery(&session_id),
            CanonicalRecoveryPlanV1::Unavailable {
                selected: Some(ref selected),
                selected_state: ref observed_state,
                ref skipped,
                blocker: CanonicalRecoveryBlockerV1::SelectedState,
            } if selected == &rev_b
                && observed_state == &selected_state
                && skipped.is_empty()
        ));
        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &selected_state
        );
    }

    #[test]
    fn recovery_plan_refuses_to_cross_a_future_logical_parent_manifest() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let rev_c = RevisionId::new("rev-c").unwrap();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        let mut future =
            serde_json::to_value(payload("store-1", "session-1", "rev-b", Some("rev-a"), 2))
                .unwrap();
        future["logical_schema"] = serde_json::json!(3);
        let candidate_b = store
            .install_revision_candidate(
                3,
                session_id.clone(),
                rev_b.clone(),
                ExpectedRevision::Exact(rev_a.clone()),
                &serde_json::to_vec(&future).unwrap(),
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session_id).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Exact(rev_a), &candidate_b)
            .unwrap();
        drop(fence);
        let candidate_c = store
            .install_revision_candidate(
                LOGICAL_SCHEMA_REVISION,
                session_id.clone(),
                rev_c.clone(),
                ExpectedRevision::Exact(rev_b.clone()),
                &encode_canonical_payload(&payload(
                    "store-1",
                    "session-1",
                    "rev-c",
                    Some("rev-b"),
                    3,
                ))
                .unwrap(),
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session_id).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Exact(rev_b.clone()), &candidate_c)
            .unwrap();
        std::fs::write(
            crate::session::physical::PhysicalLayoutV1::canonical_payload(
                root.path(),
                &session_id,
                &rev_c,
            ),
            b"tampered canonical bytes",
        )
        .unwrap();
        let selected_state = SessionLookupState::Corrupt {
            session: session_id.clone(),
            observed_revision: Some(rev_c.clone()),
            reason: CorruptSessionReason::DigestMismatch,
        };

        assert!(matches!(
            transaction.plan_last_good_recovery(&session_id),
            CanonicalRecoveryPlanV1::Unavailable {
                selected: Some(ref selected),
                selected_state: ref observed_state,
                ref skipped,
                blocker: CanonicalRecoveryBlockerV1::UnsupportedLogicalManifest {
                    revision: ref blocker_revision,
                    observed: 3,
                    supported: 2,
                },
            } if selected == &rev_c
                && observed_state == &selected_state
                && blocker_revision == &rev_b
                && recovery_skips_match(
                    skipped,
                    &[(&rev_c, CorruptSessionReason::DigestMismatch)]
                )
        ));
        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &selected_state
        );
    }

    #[test]
    fn recovery_commit_publishes_a_fresh_causal_head_with_exact_receipt() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let rev_c = RevisionId::new("rev-c").unwrap();
        let store = PhysicalFileStoreV1::create_or_open(root.path(), store_id).unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        publish_invalid_head_successor(
            &store,
            &session_id,
            &rev_b,
            ExpectedRevision::Exact(rev_a.clone()),
            2,
        );

        let commit = transaction
            .commit_last_good_recovery(
                &session_id,
                rev_c.clone(),
                "2026-08-20T10:05:00.000Z".to_string(),
            )
            .unwrap();

        assert_eq!(
            receipt(&commit).base_revision(),
            &ExpectedRevision::Exact(rev_b.clone())
        );
        assert_eq!(receipt(&commit).new_revision(), &rev_c);
        assert_eq!(receipt(&commit).durability(), CommitDurability::Confirmed);
        assert_eq!(
            receipt(&commit).head(),
            &kitsu_core::session::LogicalHead::Turn(
                kitsu_core::session::TurnId::new("turn-1").unwrap()
            )
        );
        assert!(commit.durability_warning().is_none());
        let head = store.read_head(&session_id).unwrap().unwrap();
        assert_eq!(head.revision_id(), &rev_c);
        let decoded =
            decode_canonical_payload_classified(head.candidate().canonical_bytes()).unwrap();
        assert_eq!(
            decoded.parent_revision(),
            &ExpectedRevision::Exact(rev_b.clone())
        );
        let value = serde_json::to_value(decoded).unwrap();
        assert_eq!(value["recovery_log"][0]["selected_revision"], "rev-b");
        assert_eq!(value["recovery_log"][0]["source_revision"], "rev-a");
        assert_eq!(value["recovery_log"][0]["recovery_revision"], "rev-c");
        assert_eq!(value["recovery_log"][0]["skipped"][0]["revision"], "rev-b");
        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &SessionLookupState::Present {
                revision: rev_c,
                head: kitsu_core::session::LogicalHead::Turn(
                    kitsu_core::session::TurnId::new("turn-1").unwrap()
                ),
            }
        );
        assert!(store.open_revision_envelope(&session_id, &rev_b).is_ok());
    }

    #[test]
    fn recovery_commit_candidate_cut_keeps_corrupt_head_and_exact_residue() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let rev_c = RevisionId::new("rev-c").unwrap();
        let store = PhysicalFileStoreV1::create_or_open(root.path(), store_id).unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        publish_invalid_head_successor(
            &store,
            &session_id,
            &rev_b,
            ExpectedRevision::Exact(rev_a),
            2,
        );
        transaction.inject_next_after_candidate_failure();

        let error = transaction
            .commit_last_good_recovery(
                &session_id,
                rev_c.clone(),
                "2026-08-20T10:05:00.000Z".to_string(),
            )
            .unwrap_err();

        assert_eq!(
            error.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: rev_c.clone(),
                durability: CandidateDurabilityV1::Confirmed,
            }
        );
        assert_eq!(
            error.not_published_outcome(),
            Some(&SessionCommitOutcome::NotPublished {
                store: store.store_id().clone(),
                session: session_id.clone(),
                base_revision: ExpectedRevision::Exact(rev_b.clone()),
            })
        );
        assert_eq!(
            store
                .read_head_envelope(&session_id)
                .unwrap()
                .unwrap()
                .revision_id(),
            &rev_b
        );
        assert!(store.open_revision_candidate(&session_id, &rev_c).is_ok());
        assert!(matches!(
            transaction.lookup_canonical_head(&session_id).state(),
            SessionLookupState::Corrupt { observed_revision: Some(revision), .. }
                if revision == &rev_b
        ));
    }

    #[test]
    fn recovery_commit_artifact_recheck_cut_keeps_corrupt_head() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let rev_c = RevisionId::new("rev-c").unwrap();
        let store = PhysicalFileStoreV1::create_or_open(root.path(), store_id).unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        publish_invalid_head_successor(
            &store,
            &session_id,
            &rev_b,
            ExpectedRevision::Exact(rev_a),
            2,
        );
        transaction.inject_next_artifact_recheck_failure();

        let error = transaction
            .commit_last_good_recovery(
                &session_id,
                rev_c.clone(),
                "2026-08-20T10:05:00.000Z".to_string(),
            )
            .unwrap_err();

        assert!(error.to_string().contains("artifact closure recheck"));
        assert_eq!(
            error.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: rev_c,
                durability: CandidateDurabilityV1::Confirmed,
            }
        );
        assert_eq!(
            store
                .read_head_envelope(&session_id)
                .unwrap()
                .unwrap()
                .revision_id(),
            &rev_b
        );
    }

    #[test]
    fn recovery_commit_head_sync_cut_exposes_only_new_unconfirmed_head() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let rev_c = RevisionId::new("rev-c").unwrap();
        let store = PhysicalFileStoreV1::create_or_open(root.path(), store_id).unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        publish_invalid_head_successor(
            &store,
            &session_id,
            &rev_b,
            ExpectedRevision::Exact(rev_a),
            2,
        );
        store.inject_next_head_sync_failure();

        let commit = transaction
            .commit_last_good_recovery(
                &session_id,
                rev_c.clone(),
                "2026-08-20T10:05:00.000Z".to_string(),
            )
            .unwrap();

        assert_eq!(
            receipt(&commit).durability(),
            CommitDurability::PublishedUnconfirmed
        );
        assert!(commit.durability_warning().is_some());
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_c
        );
    }

    #[test]
    fn recovery_commit_crosses_a_missing_selected_payload_without_trusting_it() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _rev_a, rev_b) = ids();
        let rev_c = RevisionId::new("rev-c").unwrap();
        let store = PhysicalFileStoreV1::create_or_open(root.path(), store_id).unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        transaction
            .commit(&payload("store-1", "session-1", "rev-b", Some("rev-a"), 2))
            .unwrap();
        std::fs::remove_file(
            crate::session::physical::PhysicalLayoutV1::canonical_payload(
                root.path(),
                &session_id,
                &rev_b,
            ),
        )
        .unwrap();

        let commit = transaction
            .commit_last_good_recovery(
                &session_id,
                rev_c.clone(),
                "2026-08-20T10:05:00.000Z".to_string(),
            )
            .unwrap();

        assert_eq!(
            receipt(&commit).base_revision(),
            &ExpectedRevision::Exact(rev_b)
        );
        assert_eq!(receipt(&commit).new_revision(), &rev_c);
        let head = store.read_head(&session_id).unwrap().unwrap();
        assert_eq!(head.revision_id(), &rev_c);
        let value: serde_json::Value =
            serde_json::from_slice(head.candidate().canonical_bytes()).unwrap();
        assert_eq!(
            value["recovery_log"][0]["skipped"][0]["reason"],
            "missing_reference"
        );
    }

    #[test]
    fn recovery_commit_without_a_verified_source_allocates_nothing() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let rev_c = RevisionId::new("rev-c").unwrap();
        let store = PhysicalFileStoreV1::create_or_open(root.path(), store_id).unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        publish_invalid_head_successor(&store, &session_id, &rev_b, ExpectedRevision::Missing, 1);

        let error = transaction
            .commit_last_good_recovery(
                &session_id,
                rev_c.clone(),
                "2026-08-20T10:05:00.000Z".to_string(),
            )
            .unwrap_err();

        assert_eq!(error.candidate_residue(), CandidateResidueV1::None);
        assert_eq!(
            store
                .read_head_envelope(&session_id)
                .unwrap()
                .unwrap()
                .revision_id(),
            &rev_b
        );
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &rev_c
            )
            .exists()
        );
    }

    #[test]
    fn canonical_lookup_maps_true_missing_and_verified_present_with_payload() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, _) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);

        let missing = transaction.lookup_canonical_head(&session_id);
        assert_eq!(
            missing.state(),
            &SessionLookupState::Missing {
                tombstone_revision: None
            }
        );
        assert!(missing.payload().is_none());

        transaction.commit(&legacy_payload("rev-a")).unwrap();
        let present = transaction.lookup_canonical_head(&session_id);
        assert_eq!(
            present.state(),
            &SessionLookupState::Present {
                revision: rev_a.clone(),
                head: kitsu_core::session::LogicalHead::Genesis
            }
        );
        assert_eq!(present.payload().unwrap().revision_id(), &rev_a);
    }

    #[test]
    fn canonical_lookup_maps_selected_logical_and_binding_corruption() {
        let mut broken =
            serde_json::to_value(payload("store-1", "session-1", "rev-b", None, 1)).unwrap();
        broken["operations"][0]["messages"]
            .as_array_mut()
            .unwrap()
            .pop();
        for (bytes, expected_reason) in [
            (
                b"not a canonical session payload".to_vec(),
                kitsu_core::session::CorruptSessionReason::Malformed,
            ),
            (
                encode_canonical_payload(&payload("other-store", "session-1", "rev-b", None, 1))
                    .unwrap(),
                kitsu_core::session::CorruptSessionReason::IdentityMismatch,
            ),
            (
                serde_json::to_vec(&broken).unwrap(),
                kitsu_core::session::CorruptSessionReason::BrokenCausality,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (store_id, session_id, _, rev_b) = ids();
            let store = crate::session::physical::PhysicalFileStoreV1::create_or_open(
                root.path(),
                store_id,
            )
            .unwrap();
            let candidate = store
                .install_revision_candidate(
                    LOGICAL_SCHEMA_REVISION,
                    session_id.clone(),
                    rev_b.clone(),
                    ExpectedRevision::Missing,
                    &bytes,
                )
                .unwrap();
            let fence = store.acquire_writer_fence(&session_id).unwrap();
            store
                .compare_and_swap_head(&fence, ExpectedRevision::Missing, &candidate)
                .unwrap();
            let transaction = CanonicalFileTransactionV1::new(&store);

            let lookup = transaction.lookup_canonical_head(&session_id);

            assert_eq!(
                lookup.state(),
                &SessionLookupState::Corrupt {
                    session: session_id,
                    observed_revision: Some(rev_b),
                    reason: expected_reason
                }
            );
            assert!(lookup.payload().is_none());
        }
    }

    #[test]
    fn canonical_lookup_maps_missing_and_digest_broken_artifacts_exactly() {
        for (fault, expected_reason) in [
            ("missing-directory", CorruptSessionReason::MissingReference),
            ("missing-content", CorruptSessionReason::MissingReference),
            ("tamper", CorruptSessionReason::DigestMismatch),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (store_id, session_id, _, rev_b) = ids();
            let store = crate::session::physical::PhysicalFileStoreV1::create_or_open(
                root.path(),
                store_id,
            )
            .unwrap();
            let transaction = CanonicalFileTransactionV1::new(&store);
            let base = legacy_payload("rev-a");
            transaction.commit(&base).unwrap();
            let artifact_bytes = b"artifact bytes";
            store
                .install_artifact(
                    1,
                    session_id.clone(),
                    "artifact-new".to_string(),
                    None,
                    artifact_bytes,
                )
                .unwrap();
            transaction
                .commit_artifact_tool_operation(
                    &base,
                    rev_b.clone(),
                    "2026-08-20T11:01:00.000Z".to_string(),
                    artifact_operation_input(
                        "artifact-new",
                        &crate::skills::content_sha256_hex(artifact_bytes),
                        artifact_bytes.len() as u64,
                        None,
                    ),
                )
                .unwrap();
            if fault == "tamper" {
                std::fs::write(
                    crate::session::physical::PhysicalLayoutV1::artifact_content(
                        root.path(),
                        &session_id,
                        "artifact-new",
                    ),
                    b"tampered bytes",
                )
                .unwrap();
            } else if fault == "missing-directory" {
                std::fs::remove_dir_all(
                    crate::session::physical::PhysicalLayoutV1::artifact_directory(
                        root.path(),
                        &session_id,
                        "artifact-new",
                    ),
                )
                .unwrap();
            } else {
                std::fs::remove_file(
                    crate::session::physical::PhysicalLayoutV1::artifact_content(
                        root.path(),
                        &session_id,
                        "artifact-new",
                    ),
                )
                .unwrap();
            }

            let lookup = transaction.lookup_canonical_head(&session_id);

            assert_eq!(
                lookup.state(),
                &SessionLookupState::Corrupt {
                    session: session_id,
                    observed_revision: Some(rev_b),
                    reason: expected_reason
                }
            );
            assert!(lookup.payload().is_none());
        }
    }

    #[test]
    fn canonical_lookup_maps_future_logical_and_physical_artifact_schemas() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let mut future =
            serde_json::to_value(payload("store-1", "session-1", "rev-b", None, 1)).unwrap();
        future["logical_schema"] = serde_json::json!(3);
        let future_bytes = serde_json::to_vec(&future).unwrap();
        let candidate = store
            .install_revision_candidate(
                3,
                session_id.clone(),
                rev_b,
                ExpectedRevision::Missing,
                &future_bytes,
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session_id).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Missing, &candidate)
            .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &SessionLookupState::Unsupported {
                dimension: SchemaDimension::Logical,
                observed_revision: 3,
                supported_revision: 2
            }
        );

        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let artifact_bytes = b"artifact bytes";
        store
            .install_artifact(
                1,
                session_id.clone(),
                "artifact-new".to_string(),
                None,
                artifact_bytes,
            )
            .unwrap();
        transaction
            .commit_artifact_tool_operation(
                &base,
                rev_b,
                "2026-08-20T11:01:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-new",
                    &crate::skills::content_sha256_hex(artifact_bytes),
                    artifact_bytes.len() as u64,
                    None,
                ),
            )
            .unwrap();
        let manifest_path = crate::session::physical::PhysicalLayoutV1::artifact_manifest(
            root.path(),
            &session_id,
            "artifact-new",
        );
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["physical_schema_revision"] = serde_json::json!(3);
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &SessionLookupState::Unsupported {
                dimension: SchemaDimension::Physical,
                observed_revision: 3,
                supported_revision: 2
            }
        );
        manifest["physical_schema_revision"] = serde_json::json!(0);
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &SessionLookupState::Corrupt {
                session: session_id,
                observed_revision: Some(RevisionId::new("rev-b").unwrap()),
                reason: CorruptSessionReason::Malformed
            }
        );
    }

    #[test]
    fn canonical_lookup_maps_future_artifact_schema() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let artifact_bytes = b"artifact bytes";
        store
            .install_artifact(
                2,
                session_id.clone(),
                "artifact-new".to_string(),
                None,
                artifact_bytes,
            )
            .unwrap();
        let successor = append_complete_operation(
            &base,
            rev_b.clone(),
            "2026-08-20T11:01:00.000Z".to_string(),
            artifact_operation_input(
                "artifact-new",
                &crate::skills::content_sha256_hex(artifact_bytes),
                artifact_bytes.len() as u64,
                None,
            ),
        )
        .unwrap();
        let candidate = store
            .install_revision_candidate(
                LOGICAL_SCHEMA_REVISION,
                session_id.clone(),
                rev_b,
                ExpectedRevision::Exact(rev_a.clone()),
                &encode_canonical_payload(&successor).unwrap(),
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session_id).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Exact(rev_a), &candidate)
            .unwrap();

        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &SessionLookupState::Unsupported {
                dimension: SchemaDimension::Artifact,
                observed_revision: 2,
                supported_revision: 1
            }
        );
    }

    #[test]
    fn canonical_head_read_accepts_exact_artifact_closure() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let artifact_bytes = b"artifact bytes";
        store
            .install_artifact(
                1,
                session_id.clone(),
                "artifact-new".to_string(),
                Some("text/plain".to_string()),
                artifact_bytes,
            )
            .unwrap();
        transaction
            .commit_artifact_tool_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-new",
                    &crate::skills::content_sha256_hex(artifact_bytes),
                    artifact_bytes.len() as u64,
                    Some("text/plain"),
                ),
            )
            .unwrap();

        let opened = transaction
            .open_canonical_head(&session_id)
            .unwrap()
            .unwrap();

        assert_eq!(opened.revision_id(), &rev_b);
        assert_eq!(opened.artifact_count(), 1);
    }

    #[test]
    fn canonical_head_read_reports_missing_artifact_without_fallback() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let artifact_bytes = b"artifact bytes";
        store
            .install_artifact(
                1,
                session_id.clone(),
                "artifact-new".to_string(),
                None,
                artifact_bytes,
            )
            .unwrap();
        transaction
            .commit_artifact_tool_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-new",
                    &crate::skills::content_sha256_hex(artifact_bytes),
                    artifact_bytes.len() as u64,
                    None,
                ),
            )
            .unwrap();
        std::fs::remove_dir_all(
            crate::session::physical::PhysicalLayoutV1::artifact_directory(
                root.path(),
                &session_id,
                "artifact-new",
            ),
        )
        .unwrap();

        let error = transaction.open_canonical_head(&session_id).unwrap_err();

        assert!(matches!(
            error,
            CanonicalHeadReadErrorV1::Artifact { ref artifact_id, .. }
                if artifact_id == "artifact-new"
        ));
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
    }

    #[test]
    fn canonical_head_read_reports_artifact_tamper_without_fallback() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let artifact_bytes = b"artifact bytes";
        store
            .install_artifact(
                1,
                session_id.clone(),
                "artifact-new".to_string(),
                None,
                artifact_bytes,
            )
            .unwrap();
        transaction
            .commit_artifact_tool_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-new",
                    &crate::skills::content_sha256_hex(artifact_bytes),
                    artifact_bytes.len() as u64,
                    None,
                ),
            )
            .unwrap();
        std::fs::write(
            crate::session::physical::PhysicalLayoutV1::artifact_content(
                root.path(),
                &session_id,
                "artifact-new",
            ),
            b"tampered bytes",
        )
        .unwrap();

        let error = transaction.open_canonical_head(&session_id).unwrap_err();

        assert!(matches!(
            error,
            CanonicalHeadReadErrorV1::Artifact { ref artifact_id, ref message, .. }
                if artifact_id == "artifact-new" && message.contains("digest mismatch")
        ));
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &SessionLookupState::Corrupt {
                session: session_id.clone(),
                observed_revision: Some(rev_b),
                reason: CorruptSessionReason::DigestMismatch
            }
        );
    }

    #[test]
    fn canonical_head_read_reports_self_consistent_physical_mismatch() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store = crate::session::physical::PhysicalFileStoreV1::create_or_open(
            root.path(),
            store_id.clone(),
        )
        .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let artifact_bytes = b"artifact bytes";
        store
            .install_artifact(
                1,
                session_id.clone(),
                "artifact-new".to_string(),
                Some("text/plain".to_string()),
                artifact_bytes,
            )
            .unwrap();
        transaction
            .commit_artifact_tool_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-new",
                    &crate::skills::content_sha256_hex(artifact_bytes),
                    artifact_bytes.len() as u64,
                    Some("text/plain"),
                ),
            )
            .unwrap();

        let replacement = b"replacement bytes";
        let replacement_manifest = crate::session::physical::ArtifactManifestV1::new(
            1,
            store_id,
            session_id.clone(),
            "artifact-new".to_string(),
            replacement.len() as u64,
            crate::session::physical::Sha256Digest::new(crate::skills::content_sha256_hex(
                replacement,
            ))
            .unwrap(),
            Some("application/octet-stream".to_string()),
        )
        .unwrap();
        std::fs::write(
            crate::session::physical::PhysicalLayoutV1::artifact_content(
                root.path(),
                &session_id,
                "artifact-new",
            ),
            replacement,
        )
        .unwrap();
        std::fs::write(
            crate::session::physical::PhysicalLayoutV1::artifact_manifest(
                root.path(),
                &session_id,
                "artifact-new",
            ),
            crate::session::physical::encode_artifact_manifest(&replacement_manifest).unwrap(),
        )
        .unwrap();
        store.open_artifact(&session_id, "artifact-new").unwrap();

        let error = transaction.open_canonical_head(&session_id).unwrap_err();

        assert!(matches!(
            error,
            CanonicalHeadReadErrorV1::Artifact { ref artifact_id, ref message, .. }
                if artifact_id == "artifact-new" && message.contains("does not match")
        ));
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
        assert_eq!(
            transaction.lookup_canonical_head(&session_id).state(),
            &SessionLookupState::Corrupt {
                session: session_id.clone(),
                observed_revision: Some(rev_b),
                reason: CorruptSessionReason::DigestMismatch
            }
        );
    }

    #[test]
    fn artifact_closed_tool_operation_publishes_exact_receipt_and_head() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let artifact_bytes = b"artifact bytes";
        store
            .install_artifact(
                1,
                session_id.clone(),
                "artifact-new".to_string(),
                Some("text/plain".to_string()),
                artifact_bytes,
            )
            .unwrap();
        let input = artifact_operation_input(
            "artifact-new",
            &crate::skills::content_sha256_hex(artifact_bytes),
            artifact_bytes.len() as u64,
            Some("text/plain"),
        );

        let commit = transaction
            .commit_artifact_tool_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                input,
            )
            .unwrap();

        assert_eq!(
            receipt(&commit).base_revision(),
            &ExpectedRevision::Exact(rev_a)
        );
        assert_eq!(receipt(&commit).new_revision(), &rev_b);
        assert_eq!(
            receipt(&commit).head(),
            &kitsu_core::session::LogicalHead::Turn(
                kitsu_core::session::TurnId::new("turn-artifact").unwrap()
            )
        );
        let head = store.read_head(&session_id).unwrap().unwrap();
        let payload =
            crate::session::canonical::decode_canonical_payload(head.candidate().canonical_bytes())
                .unwrap();
        assert_eq!(payload.revision_id(), &rev_b);
        assert_eq!(payload.artifact_count(), 1);
    }

    #[test]
    fn artifact_closure_missing_or_mismatched_is_zero_candidate() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();

        let missing = transaction
            .commit_artifact_tool_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                artifact_operation_input("artifact-new", &"a".repeat(64), 14, None),
            )
            .unwrap_err();
        assert!(missing.to_string().contains("artifact closure"));
        assert_eq!(missing.candidate_residue(), CandidateResidueV1::None);
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &rev_b
            )
            .exists()
        );

        let artifact_bytes = b"artifact bytes";
        store
            .install_artifact(
                1,
                session_id.clone(),
                "artifact-new".to_string(),
                Some("text/plain".to_string()),
                artifact_bytes,
            )
            .unwrap();
        let mismatch_revision = RevisionId::new("rev-mismatch").unwrap();
        let mismatch = transaction
            .commit_artifact_tool_operation(
                &base,
                mismatch_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-new",
                    &"b".repeat(64),
                    artifact_bytes.len() as u64,
                    Some("text/plain"),
                ),
            )
            .unwrap_err();
        assert!(mismatch.to_string().contains("does not match"));
        assert_eq!(mismatch.candidate_residue(), CandidateResidueV1::None);
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_a
        );
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &mismatch_revision
            )
            .exists()
        );

        let exact_digest = crate::skills::content_sha256_hex(artifact_bytes);
        let length_revision = RevisionId::new("rev-length-mismatch").unwrap();
        let length_mismatch = transaction
            .commit_artifact_tool_operation(
                &base,
                length_revision.clone(),
                "2026-08-20T11:03:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-new",
                    &exact_digest,
                    artifact_bytes.len() as u64 + 1,
                    Some("text/plain"),
                ),
            )
            .unwrap_err();
        assert!(length_mismatch.to_string().contains("does not match"));
        assert_eq!(
            length_mismatch.candidate_residue(),
            CandidateResidueV1::None
        );
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &length_revision
            )
            .exists()
        );

        let media_revision = RevisionId::new("rev-media-mismatch").unwrap();
        let media_mismatch = transaction
            .commit_artifact_tool_operation(
                &base,
                media_revision.clone(),
                "2026-08-20T11:04:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-new",
                    &exact_digest,
                    artifact_bytes.len() as u64,
                    Some("application/octet-stream"),
                ),
            )
            .unwrap_err();
        assert!(media_mismatch.to_string().contains("does not match"));
        assert_eq!(media_mismatch.candidate_residue(), CandidateResidueV1::None);
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &media_revision
            )
            .exists()
        );

        store
            .install_artifact(
                2,
                session_id.clone(),
                "artifact-schema".to_string(),
                None,
                artifact_bytes,
            )
            .unwrap();
        let schema_revision = RevisionId::new("rev-schema-mismatch").unwrap();
        let schema_mismatch = transaction
            .commit_artifact_tool_operation(
                &base,
                schema_revision.clone(),
                "2026-08-20T11:05:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-schema",
                    &exact_digest,
                    artifact_bytes.len() as u64,
                    None,
                ),
            )
            .unwrap_err();
        assert!(schema_mismatch.to_string().contains("does not match"));
        assert_eq!(
            schema_mismatch.candidate_residue(),
            CandidateResidueV1::None
        );
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &schema_revision
            )
            .exists()
        );
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_a
        );
    }

    #[test]
    fn artifact_closure_tamper_is_refused_before_candidate() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let artifact_bytes = b"artifact bytes";
        store
            .install_artifact(
                1,
                session_id.clone(),
                "artifact-new".to_string(),
                None,
                artifact_bytes,
            )
            .unwrap();
        std::fs::write(
            crate::session::physical::PhysicalLayoutV1::artifact_content(
                root.path(),
                &session_id,
                "artifact-new",
            ),
            b"tampered bytes",
        )
        .unwrap();

        let error = transaction
            .commit_artifact_tool_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-new",
                    &crate::skills::content_sha256_hex(artifact_bytes),
                    artifact_bytes.len() as u64,
                    None,
                ),
            )
            .unwrap_err();

        assert!(error.to_string().contains("digest mismatch"));
        assert_eq!(error.candidate_residue(), CandidateResidueV1::None);
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_a
        );
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &rev_b
            )
            .exists()
        );
    }

    #[test]
    fn artifact_closed_candidate_cut_keeps_old_head_and_artifact() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let artifact_bytes = b"artifact bytes";
        store
            .install_artifact(
                1,
                session_id.clone(),
                "artifact-new".to_string(),
                None,
                artifact_bytes,
            )
            .unwrap();
        transaction.inject_next_after_candidate_failure();

        let error = transaction
            .commit_artifact_tool_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-new",
                    &crate::skills::content_sha256_hex(artifact_bytes),
                    artifact_bytes.len() as u64,
                    None,
                ),
            )
            .unwrap_err();

        assert_eq!(
            error.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: rev_b,
                durability: CandidateDurabilityV1::Confirmed
            }
        );
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_a
        );
        assert_eq!(
            store
                .open_artifact(&session_id, "artifact-new")
                .unwrap()
                .content_bytes(),
            artifact_bytes
        );
    }

    #[test]
    fn artifact_recheck_failure_after_candidate_is_not_published() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let artifact_bytes = b"artifact bytes";
        store
            .install_artifact(
                1,
                session_id.clone(),
                "artifact-new".to_string(),
                None,
                artifact_bytes,
            )
            .unwrap();
        transaction.inject_next_artifact_recheck_failure();

        let error = transaction
            .commit_artifact_tool_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                artifact_operation_input(
                    "artifact-new",
                    &crate::skills::content_sha256_hex(artifact_bytes),
                    artifact_bytes.len() as u64,
                    None,
                ),
            )
            .unwrap_err();

        assert!(error.to_string().contains("recheck failure"));
        assert_eq!(
            error.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: rev_b.clone(),
                durability: CandidateDurabilityV1::Confirmed
            }
        );
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_a
        );
        assert!(
            crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &rev_b
            )
            .exists()
        );
        assert_eq!(
            store
                .open_artifact(&session_id, "artifact-new")
                .unwrap()
                .content_bytes(),
            artifact_bytes
        );
    }

    #[test]
    fn assembled_text_successor_publishes_exact_update_receipt_and_bytes() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        let commit = transaction
            .commit_artifact_free_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                text_operation_input(
                    "turn-new",
                    "run-new",
                    kitsu_core::RunOutcomeClass::Succeeded,
                    kitsu_core::RunAssistantDisposition::Committed,
                    "new question",
                    Some("new answer"),
                ),
            )
            .unwrap();

        assert_eq!(
            receipt(&commit).base_revision(),
            &ExpectedRevision::Exact(rev_a)
        );
        assert_eq!(receipt(&commit).new_revision(), &rev_b);
        assert_eq!(
            receipt(&commit).head(),
            &kitsu_core::session::LogicalHead::Turn(
                kitsu_core::session::TurnId::new("turn-new").unwrap()
            )
        );
        let head = store.read_head(&session_id).unwrap().unwrap();
        assert_eq!(head.revision_id(), &rev_b);
        let decoded =
            crate::session::canonical::decode_canonical_payload(head.candidate().canonical_bytes())
                .unwrap();
        assert_eq!(decoded.revision_id(), &rev_b);
        assert_eq!(decoded.parent_revision(), receipt(&commit).base_revision());
        assert_eq!(decoded.selected_head(), receipt(&commit).head());
    }

    #[test]
    fn assembled_text_successor_rejects_stale_base_before_candidate_allocation() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        transaction
            .commit_artifact_free_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                text_operation_input(
                    "turn-new",
                    "run-new",
                    kitsu_core::RunOutcomeClass::Succeeded,
                    kitsu_core::RunAssistantDisposition::Committed,
                    "new question",
                    Some("new answer"),
                ),
            )
            .unwrap();
        let stale_revision = RevisionId::new("rev-stale").unwrap();

        let error = transaction
            .commit_artifact_free_operation(
                &base,
                stale_revision.clone(),
                "2026-08-20T11:02:00.000Z".to_string(),
                text_operation_input(
                    "turn-stale",
                    "run-stale",
                    kitsu_core::RunOutcomeClass::ProviderFault,
                    kitsu_core::RunAssistantDisposition::Absent,
                    "stale question",
                    None,
                ),
            )
            .unwrap_err();

        assert!(error.to_string().contains("conflict"));
        assert_eq!(error.candidate_residue(), CandidateResidueV1::None);
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &stale_revision
            )
            .exists()
        );
    }

    #[test]
    fn assembled_text_successor_candidate_cut_keeps_old_head_and_exact_residue() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        transaction.inject_next_after_candidate_failure();

        let error = transaction
            .commit_artifact_free_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                text_operation_input(
                    "turn-new",
                    "run-new",
                    kitsu_core::RunOutcomeClass::Succeeded,
                    kitsu_core::RunAssistantDisposition::Committed,
                    "new question",
                    Some("new answer"),
                ),
            )
            .unwrap_err();

        assert_eq!(
            error.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: rev_b.clone(),
                durability: CandidateDurabilityV1::Confirmed
            }
        );
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_a
        );
        assert!(store.open_revision_candidate(&session_id, &rev_b).is_ok());
    }

    #[test]
    fn assembled_text_successor_head_sync_cut_is_new_visible_unconfirmed() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let base = legacy_payload("rev-a");
        transaction.commit(&base).unwrap();
        store.inject_next_head_sync_failure();

        let commit = transaction
            .commit_artifact_free_operation(
                &base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                text_operation_input(
                    "turn-new",
                    "run-new",
                    kitsu_core::RunOutcomeClass::Succeeded,
                    kitsu_core::RunAssistantDisposition::Committed,
                    "new question",
                    Some("new answer"),
                ),
            )
            .unwrap();

        assert_eq!(
            receipt(&commit).durability(),
            CommitDurability::PublishedUnconfirmed
        );
        assert!(commit.durability_warning().is_some());
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_b
        );
    }

    #[test]
    fn assembled_text_successor_refuses_artifact_bearing_base_before_effects() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let mut value =
            serde_json::to_value(payload("store-1", "session-1", "rev-a", None, 1)).unwrap();
        value["artifact_manifest"] = serde_json::json!([{
            "artifact_schema":1,
            "artifact_id":"artifact-old",
            "sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bytes":4,
            "media_type":"text/plain"
        }]);
        value["operations"][0]["retained_effects"] =
            serde_json::json!([{"effect_id":"effect-old","artifact_ids":["artifact-old"]}]);
        let artifact_base = crate::session::canonical::decode_canonical_payload(
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();

        let error = transaction
            .commit_artifact_free_operation(
                &artifact_base,
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
                text_operation_input(
                    "turn-new",
                    "run-new",
                    kitsu_core::RunOutcomeClass::Succeeded,
                    kitsu_core::RunAssistantDisposition::Committed,
                    "new question",
                    Some("new answer"),
                ),
            )
            .unwrap_err();

        assert!(error.to_string().contains("physical artifact closure"));
        assert!(error.not_published_outcome().is_none());
        assert!(store.read_head(&session_id).unwrap().is_none());
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &rev_b
            )
            .exists()
        );
    }

    #[test]
    fn legacy_migration_publishes_exact_baseline_without_mutating_source() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, _) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let source = closed_current_v1();
        let source_before = source.clone();

        let committed = transaction
            .commit_current_v1_legacy_baseline(
                &source,
                "session-1",
                rev_a.clone(),
                "2026-08-20T11:00:00.000Z".to_string(),
            )
            .unwrap();

        assert_eq!(source, source_before);
        assert_eq!(receipt(&committed).new_revision(), &rev_a);
        assert_eq!(
            receipt(&committed).base_revision(),
            &ExpectedRevision::Missing
        );
        let head = store.read_head(&session_id).unwrap().unwrap();
        assert_eq!(head.revision_id(), &rev_a);
        let expected = crate::session::canonical::prepare_current_v1_legacy_baseline(
            &source,
            "session-1",
            store.store_id().clone(),
            rev_a,
            "2026-08-20T11:00:00.000Z".to_string(),
        )
        .unwrap();
        assert_eq!(
            head.candidate().canonical_bytes(),
            encode_canonical_payload(&expected).unwrap()
        );
    }

    #[test]
    fn legacy_migration_is_no_clobber_when_already_present() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let source = closed_current_v1();
        transaction
            .commit_current_v1_legacy_baseline(
                &source,
                "session-1",
                rev_a.clone(),
                "2026-08-20T11:00:00.000Z".to_string(),
            )
            .unwrap();

        let error = transaction
            .commit_current_v1_legacy_baseline(
                &source,
                "session-1",
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
            )
            .unwrap_err();

        assert!(error.to_string().contains("conflict"));
        assert_eq!(error.candidate_residue(), CandidateResidueV1::None);
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_a
        );
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &rev_b
            )
            .exists()
        );
    }

    #[test]
    fn legacy_migration_preserves_a_conflicting_orphan_and_source() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, _) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let different = payload("store-1", "session-1", "rev-a", None, 1);
        let different_bytes = encode_canonical_payload(&different).unwrap();
        transaction.inject_next_after_candidate_failure();
        transaction.commit(&different).unwrap_err();
        let source = closed_current_v1();
        let source_before = source.clone();

        let error = transaction
            .commit_current_v1_legacy_baseline(
                &source,
                "session-1",
                rev_a.clone(),
                "2026-08-20T11:00:00.000Z".to_string(),
            )
            .unwrap_err();

        assert_eq!(source, source_before);
        assert_eq!(
            error.candidate_residue(),
            CandidateResidueV1::PreexistingCollision {
                revision: rev_a.clone()
            }
        );
        assert!(store.read_head(&session_id).unwrap().is_none());
        assert_eq!(
            store
                .open_revision_candidate(&session_id, &rev_a)
                .unwrap()
                .canonical_bytes(),
            different_bytes
        );
    }

    #[test]
    fn legacy_migration_refusal_and_candidate_cut_never_change_source_or_head() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let unresolved = current_v1(serde_json::json!([
            {"role":"user","content":"inspect"},
            {"role":"assistant","content":"","tool_calls":[{
                "tool_name":"fs_read_file","args":"{}","call_id":"call-1"
            }]},
            {"role":"tool","tool_call_id":"call-1","content":
                "[tool output 9.0 KB — full result saved to /private/session-1.artifacts/call-1.txt.]"},
            {"role":"assistant","content":"done"}
        ]));
        let unresolved_before = unresolved.clone();
        let refused = transaction
            .commit_current_v1_legacy_baseline(
                &unresolved,
                "session-1",
                rev_a.clone(),
                "2026-08-20T11:00:00.000Z".to_string(),
            )
            .unwrap_err();
        assert!(refused.to_string().contains("artifact closure"));
        assert!(refused.not_published_outcome().is_none());
        assert_eq!(unresolved, unresolved_before);
        assert!(store.read_head(&session_id).unwrap().is_none());
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &rev_a
            )
            .exists()
        );

        let source = closed_current_v1();
        let source_before = source.clone();
        transaction.inject_next_after_candidate_failure();
        let cut = transaction
            .commit_current_v1_legacy_baseline(
                &source,
                "session-1",
                rev_b.clone(),
                "2026-08-20T11:01:00.000Z".to_string(),
            )
            .unwrap_err();
        assert_eq!(source, source_before);
        assert_eq!(
            cut.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: rev_b.clone(),
                durability: CandidateDurabilityV1::Confirmed
            }
        );
        assert!(store.read_head(&session_id).unwrap().is_none());
        assert!(store.open_revision_candidate(&session_id, &rev_b).is_ok());
    }

    #[test]
    fn create_and_update_publish_the_exact_validated_payload() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, rev_b) = ids();
        let store = crate::session::physical::PhysicalFileStoreV1::create_or_open(
            root.path(),
            store_id.clone(),
        )
        .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);

        let first = payload("store-1", "session-1", "rev-a", None, 1);
        let first_outcome = transaction.commit(&first).unwrap();
        assert_eq!(
            receipt(&first_outcome).durability(),
            CommitDurability::Confirmed
        );
        assert_eq!(receipt(&first_outcome).new_revision(), &rev_a);

        let second = payload("store-1", "session-1", "rev-b", Some("rev-a"), 2);
        let second_outcome = transaction.commit(&second).unwrap();
        assert_eq!(receipt(&second_outcome).new_revision(), &rev_b);
        let head = store.read_head(&session_id).unwrap().unwrap();
        assert_eq!(head.revision_id(), &rev_b);
        assert_eq!(
            head.candidate().canonical_bytes(),
            crate::session::canonical::encode_canonical_payload(&second).unwrap()
        );
    }

    #[test]
    fn stale_transaction_is_rejected_before_allocating_an_orphan() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, _, _) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        transaction
            .commit(&payload("store-1", "session-1", "rev-b", Some("rev-a"), 2))
            .unwrap();

        let stale = payload("store-1", "session-1", "rev-stale", Some("rev-a"), 2);
        let error = transaction.commit(&stale).unwrap_err();
        assert!(error.to_string().contains("conflict"));
        assert_eq!(error.candidate_residue(), CandidateResidueV1::None);
        assert!(matches!(
            error.not_published_outcome(),
            Some(SessionCommitOutcome::NotPublished {
                base_revision: ExpectedRevision::Exact(base),
                ..
            }) if base == &RevisionId::new("rev-a").unwrap()
        ));
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &RevisionId::new("rev-b").unwrap()
        );
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &RevisionId::new("rev-stale").unwrap()
            )
            .exists()
        );
    }

    #[test]
    fn cut_after_candidate_reports_a_confirmed_unselected_orphan() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, _) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        transaction.inject_next_after_candidate_failure();

        let error = transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap_err();
        assert_eq!(
            error.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: rev_a.clone(),
                durability: CandidateDurabilityV1::Confirmed,
            }
        );
        assert!(store.read_head(&session_id).unwrap().is_none());
        assert!(store.open_revision_candidate(&session_id, &rev_a).is_ok());
    }

    #[test]
    fn retry_after_orphan_reports_exact_or_conflicting_candidate_collision() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, _) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);
        let first = payload("store-1", "session-1", "rev-a", None, 1);
        transaction.inject_next_after_candidate_failure();
        transaction.commit(&first).unwrap_err();

        let exact = transaction.commit(&first).unwrap_err();
        assert_eq!(
            exact.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: rev_a.clone(),
                durability: CandidateDurabilityV1::Confirmed,
            }
        );

        let different = payload("store-1", "session-1", "rev-a", None, 2);
        let collision = transaction.commit(&different).unwrap_err();
        assert_eq!(
            collision.candidate_residue(),
            CandidateResidueV1::PreexistingCollision {
                revision: rev_a.clone(),
            }
        );
        assert!(store.read_head(&session_id).unwrap().is_none());
        assert_eq!(
            store
                .open_revision_candidate(&session_id, &rev_a)
                .unwrap()
                .canonical_bytes(),
            encode_canonical_payload(&first).unwrap()
        );
    }

    #[test]
    fn candidate_sync_cut_is_not_published_and_preserves_unconfirmed_residue() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, _) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        store.inject_next_target_sync_failure();
        let transaction = CanonicalFileTransactionV1::new(&store);

        let error = transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap_err();
        assert_eq!(
            error.candidate_residue(),
            CandidateResidueV1::InstalledUnselected {
                revision: rev_a,
                durability: CandidateDurabilityV1::Unconfirmed,
            }
        );
        assert!(store.read_head(&session_id).unwrap().is_none());
    }

    #[test]
    fn head_sync_cut_is_new_visible_and_published_unconfirmed() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, _) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        store.inject_next_head_sync_failure();
        let transaction = CanonicalFileTransactionV1::new(&store);

        let outcome = transaction
            .commit(&payload("store-1", "session-1", "rev-a", None, 1))
            .unwrap();
        assert_eq!(
            receipt(&outcome).durability(),
            CommitDurability::PublishedUnconfirmed
        );
        assert!(outcome
            .durability_warning()
            .unwrap()
            .contains("injected post-install failure"));
        assert_eq!(
            store.read_head(&session_id).unwrap().unwrap().revision_id(),
            &rev_a
        );
    }

    #[test]
    fn store_identity_mismatch_fails_before_fence_candidate_or_head() {
        let root = tempfile::tempdir().unwrap();
        let (store_id, session_id, rev_a, _) = ids();
        let store =
            crate::session::physical::PhysicalFileStoreV1::create_or_open(root.path(), store_id)
                .unwrap();
        let transaction = CanonicalFileTransactionV1::new(&store);

        let error = transaction
            .commit(&payload("store-other", "session-1", "rev-a", None, 1))
            .unwrap_err();
        assert!(error.to_string().contains("store identity"));
        assert_eq!(error.candidate_residue(), CandidateResidueV1::None);
        assert!(store.read_head(&session_id).unwrap().is_none());
        assert!(
            !crate::session::physical::PhysicalLayoutV1::revision_directory(
                root.path(),
                &session_id,
                &rev_a
            )
            .exists()
        );
    }
}
