//! Provider-free composition of canonical bytes and physical publication.

use std::fmt;

#[cfg(test)]
use std::cell::Cell;

use kitsu_core::session::{
    CommitDurability, ExpectedRevision, RevisionId, SessionCommitOutcome, SessionCommitReceipt,
    SessionId, SessionLookupState,
};

use super::canonical::{
    append_complete_operation, encode_canonical_payload, prepare_current_v1_legacy_baseline,
    CanonicalOperationInputV1, CanonicalSessionRevisionV1,
};
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
