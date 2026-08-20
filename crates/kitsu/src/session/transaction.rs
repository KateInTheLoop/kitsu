//! Provider-free composition of canonical bytes and physical publication.

use std::fmt;

#[cfg(test)]
use std::cell::Cell;

use kitsu_core::session::{
    CommitDurability, ExpectedRevision, RevisionId, SessionCommitOutcome, SessionCommitReceipt,
    SessionId, SessionLookupState,
};

use super::canonical::{encode_canonical_payload, CanonicalSessionRevisionV1};
use super::physical::{PhysicalCandidateInstallError, PhysicalFileStoreV1, PhysicalHeadCasError};

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

#[derive(Debug)]
struct CanonicalTransactionErrorV1 {
    message: String,
    candidate_residue: CandidateResidueV1,
    not_published: Option<Box<SessionCommitOutcome>>,
}

impl CanonicalTransactionErrorV1 {
    fn before_identity(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            candidate_residue: CandidateResidueV1::None,
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
}

impl<'a> CanonicalFileTransactionV1<'a> {
    fn new(store: &'a PhysicalFileStoreV1) -> Self {
        Self {
            store,
            #[cfg(test)]
            fail_next_after_candidate: Cell::new(false),
        }
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
                message,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use kitsu_core::session::{
        CommitDurability, ExpectedRevision, RevisionId, SessionCommitReceipt, SessionId, StoreId,
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
            "logical_schema": 1,
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
            "compaction_binding": null
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
