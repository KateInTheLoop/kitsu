//! Pure admission between the current-v1 snapshot and canonical session store.
//!
//! This module owns no paths and performs no I/O. It deliberately classifies
//! already-observed values before a later card is allowed to mount either
//! representation for production reads or writes.

use kitsu_core::session::{LogicalHead, RevisionId, SessionLookupState};

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

#[cfg(test)]
mod tests {
    use kitsu_core::session::{
        CorruptSessionReason, ExpectedRevision, LogicalHead, RevisionId, SchemaDimension,
        SessionId, SessionLookupState, TurnId,
    };

    use super::{
        admit_session_mount_v1, CurrentV1MountFaultV1, CurrentV1MountObservationV1,
        SessionMountAdmissionV1, SessionMountRejectionV1,
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
}
